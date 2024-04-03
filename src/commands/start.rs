// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2024 Akira Moroo

use std::{os::unix::net::UnixStream, path::PathBuf};

use anyhow::Result;
use liboci_cli::Start;

use crate::{api, traits::WriteTo};

pub fn start(args: Start, _root_path: PathBuf, vmm_sock: &mut UnixStream) -> Result<()> {
    let request = api::Request {
        container_id: args.container_id.clone(),
        command: api::Command::Start,
        vm_config: None,
    };

    request.send(vmm_sock)?;

    Ok(())
}
