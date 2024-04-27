// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2024 Akira Moroo

//! # Akari Virtual Machine
//! This is a daemon that listens for requests from the akari OCI runtime to manage containers.

use std::{
    collections::HashMap,
    future::Future,
    os::{
        fd::AsRawFd,
        unix::{fs::FileTypeExt, net::UnixStream},
    },
    path::PathBuf,
    sync::Arc,
};

use anyhow::Result;
use clap::Parser;

use futures::{future, stream::StreamExt};
use libakari::{
    api::{self, Api, Command, Response},
    path::{root_path, vmm_sock_path},
    vm_config::{load_vm_config, MacosVmConfig, MacosVmSerial},
};
use log::{debug, error, info};
use tarpc::{
    serde_transport,
    server::{self, Channel},
    tokio_serde::formats::Json,
};
use tokio::{
    runtime::Runtime,
    sync::{mpsc, RwLock},
    task::JoinHandle,
};

#[derive(clap::Parser)]
struct Opts {
    /// root directory to store container state
    #[clap(short, long)]
    pub root: Option<PathBuf>,
    /// Specify the path to the VMM socket
    #[clap(short, long)]
    vmm_sock: Option<PathBuf>,
    /// Specify the path to the VM console socket
    #[clap(short, long)]
    console_sock: Option<PathBuf>,
}

#[derive(Debug)]
struct ContainerState {
    bundle: PathBuf,
    status: api::VmStatus, // TODO: Use ContainerStatus
    vsock_port: u32,
}

type ContainerStateMap = HashMap<String, ContainerState>;
type VsockRx = mpsc::Receiver<(u32, Vec<u8>)>;

#[derive(Clone)]
struct ApiServer {
    state_map: Arc<RwLock<ContainerStateMap>>,
    cmd_tx: mpsc::Sender<Command>,
    data_rx: Arc<RwLock<VsockRx>>,
}

impl Api for ApiServer {
    async fn create(
        self,
        _context: ::tarpc::context::Context,
        container_id: String,
        req: api::CreateRequest,
    ) -> Result<(), api::Error> {
        info!(
            "create: container_id={}, bundle={:?}",
            container_id, req.bundle
        );

        let mut state_map = self.state_map.write().await;

        if state_map.contains_key(&container_id) {
            return Err(api::Error::ContainerAlreadyExists);
        }

        // Find the smallest used vsock port
        const DEFAULT_MIN_PORT: u32 = 1234;
        let mut port = DEFAULT_MIN_PORT - 1;
        state_map.values().for_each(|state| {
            port = std::cmp::max(port, state.vsock_port);
        });
        port += 1;

        let req_str = serde_json::to_string(&req).unwrap();

        self.cmd_tx
            .send(Command::Connect(port))
            .await
            .map_err(|_| api::Error::VmCommandFailed)?;
        self.cmd_tx
            .send(Command::VsockSend(port, req_str.as_bytes().to_vec()))
            .await
            .map_err(|_| api::Error::VmCommandFailed)?;
        let mut data_rx = self.data_rx.write().await;
        let (port, _data) = data_rx.recv().await.unwrap();

        let state = ContainerState {
            bundle: req.bundle.clone(),
            status: api::VmStatus::Creating,
            vsock_port: port,
        };

        state_map.insert(container_id.clone(), state);

        Ok(())
    }

    async fn delete(
        self,
        _context: ::tarpc::context::Context,
        container_id: String,
    ) -> Result<(), api::Error> {
        info!("delete: container_id={}", container_id);

        let mut state_map = self.state_map.write().await;
        let state = state_map
            .get_mut(&container_id)
            .ok_or(api::Error::ContainerNotFound)?;

        match state.status {
            api::VmStatus::Created | api::VmStatus::Stopped => {
                let msg = "delete".as_bytes().to_vec(); // TODO
                self.cmd_tx
                    .send(Command::VsockSend(state.vsock_port, msg))
                    .await
                    .map_err(|_| api::Error::VmCommandFailed)?;
                self.cmd_tx
                    .send(Command::Disconnect(state.vsock_port))
                    .await
                    .map_err(|_| api::Error::VmCommandFailed)?;
                state_map.remove(&container_id);
                Ok(())
            }
            _ => Err(api::Error::UnpextectedContainerStatus(state.status.clone())),
        }
    }

    async fn kill(
        self,
        _context: ::tarpc::context::Context,
        container_id: String,
    ) -> Result<(), api::Error> {
        info!("kill: container_id={}", container_id);

        let mut state_map = self.state_map.write().await;
        let state = state_map
            .get_mut(&container_id)
            .ok_or(api::Error::ContainerNotFound)?;

        match state.status {
            api::VmStatus::Created | api::VmStatus::Running => {
                let msg = "kill".as_bytes().to_vec(); // TODO
                self.cmd_tx
                    .send(Command::VsockSend(state.vsock_port, msg))
                    .await
                    .map_err(|_| api::Error::VmCommandFailed)?;
                state.status = api::VmStatus::Stopped;
                Ok(())
            }
            _ => Err(api::Error::UnpextectedContainerStatus(state.status.clone())),
        }
    }

    async fn start(
        self,
        _context: ::tarpc::context::Context,
        container_id: String,
    ) -> Result<(), api::Error> {
        info!("start: container_id={}", container_id);

        let mut state_map = self.state_map.write().await;
        let state = state_map
            .get_mut(&container_id)
            .ok_or(api::Error::ContainerNotFound)?;

        match state.status {
            api::VmStatus::Created => {
                let msg = "start".as_bytes().to_vec(); // TODO
                self.cmd_tx
                    .send(Command::VsockSend(state.vsock_port, msg))
                    .await
                    .map_err(|_| api::Error::VmCommandFailed)?;
                state.status = api::VmStatus::Running;
                Ok(())
            }
            _ => Err(api::Error::UnpextectedContainerStatus(state.status.clone())),
        }
    }

    async fn state(
        self,
        _context: ::tarpc::context::Context,
        container_id: String,
    ) -> Result<Response, api::Error> {
        info!("state: container_id={}", container_id);

        let state_map = self.state_map.read().await;
        let state = state_map
            .get(&container_id)
            .ok_or(api::Error::ContainerNotFound)?;

        let msg = "state".as_bytes().to_vec(); // TODO
        self.cmd_tx
            .send(Command::VsockSend(state.vsock_port, msg))
            .await
            .map_err(|_| api::Error::VmCommandFailed)?;

        // TODO: Get the actual PID
        let response = api::Response {
            container_id,
            status: state.status.clone(),
            pid: None,
            bundle: state.bundle.clone(),
        };
        Ok(response)
    }

    async fn connect(
        self,
        _context: ::tarpc::context::Context,
        container_id: String,
        _port: u32,
    ) -> Result<(), api::Error> {
        info!("connect: container_id={}", container_id);

        let mut state_map = self.state_map.write().await;
        let state = state_map
            .get_mut(&container_id)
            .ok_or(api::Error::ContainerNotFound)?;

        match state.status {
            api::VmStatus::Running => {
                // TODO: Implement the container connect process
                Ok(())
            }
            _ => Err(api::Error::UnpextectedContainerStatus(state.status.clone())),
        }
    }
}

async fn handle_cmd(
    vm: &mut vmm::vm::Vm,
    cmd_rx: &mut mpsc::Receiver<Command>,
    data_tx: &mut mpsc::Sender<(u32, Vec<u8>)>,
) -> Result<()> {
    loop {
        debug!("Waiting for command...");
        let cmd = cmd_rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Command channel closed"))?;
        match cmd {
            api::Command::Start => vm.start()?,
            api::Command::Kill => vm.kill()?,
            api::Command::Connect(port) => vm.connect(port)?,
            api::Command::Disconnect(port) => vm.disconnect(port)?,
            api::Command::VsockSend(port, data) => vm.vsock_send(port, data)?,
            api::Command::VsockRecv(port) => {
                let mut data = Vec::new();
                vm.vsock_recv(port, &mut data)?;
                data_tx.send((port, data)).await?;
            }
            _ => {
                error!("Unexpected command");
                return Err(anyhow::anyhow!("Unexpected command"));
            }
        }
    }
    #[allow(unreachable_code)]
    Ok(())
}

fn vm_thread(
    vm_config: MacosVmConfig,
    cmd_rx: &mut mpsc::Receiver<Command>,
    data_tx: &mut mpsc::Sender<(u32, Vec<u8>)>,
) -> Result<()> {
    let serial_sock = match &vm_config.serial {
        Some(serial) => Some(UnixStream::connect(&serial.path)?),
        None => None,
    };

    let config = vmm::config::Config::from_vm_config(vm_config)?
        .console(serial_sock.as_ref().map(|s| s.as_raw_fd()))?
        .build();
    let mut vm = vmm::vm::Vm::new(config)?;

    let rt = Runtime::new().expect("Failed to create a runtime.");
    rt.block_on(handle_cmd(&mut vm, cmd_rx, data_tx))
        .unwrap_or_else(|e| panic!("{}", e));

    Ok(())
}

async fn create_vm(
    vm_config: MacosVmConfig,
) -> Result<(
    JoinHandle<Result<(), anyhow::Error>>,
    mpsc::Sender<Command>,
    mpsc::Receiver<(u32, Vec<u8>)>,
)> {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<api::Command>(8);
    let (mut data_tx, data_rx) = mpsc::channel::<(u32, Vec<u8>)>(8);

    let thread = tokio::spawn(async move { vm_thread(vm_config, &mut cmd_rx, &mut data_tx) });

    Ok((thread, cmd_tx, data_rx))
}

async fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
    tokio::spawn(fut);
}

#[tokio::main]

async fn main() -> Result<()> {
    env_logger::init();

    let opts = Opts::parse();

    let root_path = root_path(opts.root)?;
    let vmm_sock_path = vmm_sock_path(&root_path, opts.vmm_sock);

    match vmm_sock_path.try_exists() {
        Ok(exist) => {
            if exist {
                let metadata = std::fs::metadata(&vmm_sock_path)?;
                if metadata.file_type().is_socket() {
                    std::fs::remove_file(&vmm_sock_path)?;
                } else {
                    anyhow::bail!("VMM socket path exists and is not a socket");
                }
            }
        }
        Err(e) => {
            anyhow::bail!("Failed to check if VMM socket path exists: {}", e);
        }
    }

    let console_path = opts
        .console_sock
        .unwrap_or_else(|| root_path.join("console.sock"));

    let vm_config_path = root_path.join("vm.json");
    let mut vm_config = load_vm_config(&vm_config_path)?;
    vm_config.serial = Some(MacosVmSerial { path: console_path });

    let (thread, cmd_tx, data_rx) = create_vm(vm_config).await?;
    info!("VM thread created");

    let data_rx = Arc::new(RwLock::new(data_rx));

    info!("Starting VM");
    cmd_tx.send(api::Command::Start).await?;

    info!("Listening on: {:?}", vmm_sock_path);
    let mut listener = serde_transport::unix::listen(vmm_sock_path, Json::default).await?;
    listener.config_mut().max_frame_length(usize::MAX);

    let state_map = Arc::new(RwLock::new(HashMap::new()));

    listener
        .filter_map(|r| future::ready(r.ok()))
        .map(server::BaseChannel::with_defaults)
        .map(|channel| {
            debug!("Accepted connection");
            let state_map = state_map.clone();
            let cmd_tx = cmd_tx.clone();
            let data_rx = data_rx.clone();
            let server = ApiServer {
                state_map,
                cmd_tx,
                data_rx,
            };
            channel.execute(server.serve()).for_each(spawn)
        })
        .buffer_unordered(10)
        .for_each(|_| async {})
        .await;

    thread.await??;

    Ok(())
}
