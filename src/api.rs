// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2024 Akira Moroo

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::traits::{ReadFrom, WriteTo};
use crate::vmm::api::MacosVmConfig;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Command {
    Create,
    Delete,
    Kill,
    Start,
    State,
}

impl WriteTo for Command {}
impl ReadFrom for Command {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VmStatus {
    Creating,
    Created,
    Running,
    Stopped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub container_id: String,
    pub command: Command,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vm_config: Option<MacosVmConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub container_id: String,
    pub status: VmStatus,
    pub pid: Option<i32>,
    pub config: MacosVmConfig,
    pub bundle: PathBuf,
}

#[tarpc::service]
pub trait BackendApi {
    async fn create(container_id: String, vm_config: MacosVmConfig, bundle: PathBuf);
    async fn delete(container_id: String);
    async fn kill(container_id: String);
    async fn start(container_id: String);
    async fn state(container_id: String) -> Response;
}
