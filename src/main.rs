mod client;

use anyhow::{Result, bail};
use client::{ProxmoxClient, https_url, segment, volid};
use reqwest::Method;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
struct ProxmoxServer {
    client: Arc<Result<ProxmoxClient, String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum GuestKind {
    /// Virtual machine.
    Qemu,
    /// Container.
    Lxc,
}

impl GuestKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Qemu => "qemu",
            Self::Lxc => "lxc",
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct NodeParams {
    /// Node name, as returned by `list_nodes`.
    node: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct StorageContentParams {
    node: String,
    /// Storage ID, as returned by `list_storages`.
    storage: String,
    /// Filter: `iso`, `vztmpl` (container templates), `backup`, `images` or `rootdir`.
    content: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TaskParams {
    /// Task ID, starting with `UPID:`.
    upid: String,
}

/// Identifies one VM or container.
#[derive(Deserialize, schemars::JsonSchema)]
struct GuestParams {
    node: String,
    vmid: u32,
    kind: GuestKind,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ChecksumAlgorithm {
    Sha256,
    Sha512,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DownloadIsoParams {
    node: String,
    /// Storage that accepts ISO images.
    storage: String,
    /// HTTPS URL of the ISO.
    url: String,
    /// File name to store, ending in `.iso`.
    filename: String,
    /// Checksum published by the distribution. Mandatory: the download is rejected if it does not match.
    checksum: String,
    checksum_algorithm: ChecksumAlgorithm,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DownloadLxcParams {
    node: String,
    /// Storage that accepts container templates.
    storage: String,
    /// Template name from `list_lxc_catalog`, e.g. `debian-12-standard_12.7-1_amd64.tar.zst`.
    template: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CreateContainerParams {
    node: String,
    /// New container ID.
    vmid: u32,
    /// Template volume ID, e.g. `local:vztmpl/debian-12-standard_12.7-1_amd64.tar.zst`.
    ostemplate: String,
    hostname: String,
    /// Storage for the root disk.
    storage: String,
    /// Root disk size in GB (default 8).
    disk_gb: Option<u32>,
    /// Memory in MB (default 512).
    memory_mb: Option<u32>,
    /// Number of cores (default 1).
    cores: Option<u32>,
    /// Bridge, DHCP is used for the address (default `vmbr0`).
    bridge: Option<String>,
    /// SSH public keys allowed to log in as root.
    ssh_public_keys: Option<String>,
    /// Unprivileged container (default true; leave it on).
    unprivileged: Option<bool>,
    /// Enable nesting, needed by systemd 257+ (Debian 13) to run cleanly (default false).
    nesting: Option<bool>,
    /// Start right after creation (default false).
    start: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CreateVmParams {
    node: String,
    vmid: u32,
    name: String,
    /// Storage for the disk.
    storage: String,
    /// Disk size in GB (default 32).
    disk_gb: Option<u32>,
    /// Memory in MB (default 2048).
    memory_mb: Option<u32>,
    /// Number of cores (default 2).
    cores: Option<u32>,
    /// Bridge (default `vmbr0`).
    bridge: Option<String>,
    /// ISO volume ID to boot the installer from, e.g. `local:iso/debian.iso`.
    iso: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CloneParams {
    node: String,
    /// Source VM or container, usually a template.
    vmid: u32,
    kind: GuestKind,
    /// ID of the new guest.
    newid: u32,
    /// Name (VM) or hostname (container) of the new guest.
    name: String,
    /// Full clone instead of linked clone (default true: independent of the template).
    full: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SnapshotCreateParams {
    node: String,
    vmid: u32,
    kind: GuestKind,
    snapname: String,
    description: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SnapshotParams {
    node: String,
    vmid: u32,
    kind: GuestKind,
    snapname: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum BackupMode {
    /// No downtime, uses a snapshot.
    Snapshot,
    /// Briefly suspends the guest.
    Suspend,
    /// Stops the guest during the backup.
    Stop,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct BackupParams {
    node: String,
    vmid: u32,
    /// Storage that accepts backups.
    storage: String,
    /// Default `snapshot`.
    mode: Option<BackupMode>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RestoreParams {
    node: String,
    /// ID of the guest to create from the backup.
    vmid: u32,
    kind: GuestKind,
    /// Backup volume ID, from `list_storage_content` with content `backup`.
    archive: String,
    /// Storage for the restored disks.
    storage: Option<String>,
    /// Overwrite an existing guest with this ID (default false). Destructive.
    force: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum PowerAction {
    Start,
    /// Clean shutdown.
    Shutdown,
    /// Immediate power off.
    Stop,
    Reboot,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PowerParams {
    node: String,
    vmid: u32,
    kind: GuestKind,
    action: PowerAction,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DeleteGuestParams {
    node: String,
    vmid: u32,
    kind: GuestKind,
    /// Must repeat `vmid`, as a safety check.
    confirm_vmid: u32,
    /// Also remove it from backup jobs and replication (default true).
    purge: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DeleteVolumeParams {
    node: String,
    storage: String,
    /// Volume ID, e.g. `local:backup/vzdump-lxc-100-2026_10_28-12_00_00.tar.zst`.
    volid: String,
    /// Must repeat `volid`, as a safety check.
    confirm_volid: String,
}

fn pretty(v: &Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(v)?)
}

fn render(r: Result<String>) -> String {
    r.unwrap_or_else(|e| format!("Error: {e:#}"))
}

/// Path of a guest, with validated components.
fn guest_path(node: &str, kind: &GuestKind, vmid: u32) -> Result<String> {
    Ok(format!("/nodes/{}/{}/{vmid}", segment(node)?, kind.as_str()))
}

fn opt(params: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<String>) {
    if let Some(v) = value {
        params.push((key, v));
    }
}

fn flag(b: bool) -> String {
    if b { "1" } else { "0" }.to_owned()
}

#[tool_router]
impl ProxmoxServer {
    fn c(&self) -> Result<&ProxmoxClient> {
        match self.client.as_ref() {
            Ok(c) => Ok(c),
            Err(e) => bail!("{e}"),
        }
    }

    // ---- Read-only ----

    #[tool(
        description = "List the nodes of the Proxmox cluster with their status and resource usage",
        annotations(read_only_hint = true)
    )]
    async fn list_nodes(&self) -> String {
        render(async { pretty(&self.c()?.get("/nodes", &[]).await?) }.await)
    }

    #[tool(
        description = "List all VMs and containers in the cluster with node, status and resource usage",
        annotations(read_only_hint = true)
    )]
    async fn list_guests(&self) -> String {
        render(
            async { pretty(&self.c()?.get("/cluster/resources", &[("type", "vm".into())]).await?) }
                .await,
        )
    }

    #[tool(
        description = "Get the current status of one VM or container",
        annotations(read_only_hint = true)
    )]
    async fn get_guest_status(&self, Parameters(p): Parameters<GuestParams>) -> String {
        render(
            async {
                let path = format!("{}/status/current", guest_path(&p.node, &p.kind, p.vmid)?);
                pretty(&self.c()?.get(&path, &[]).await?)
            }
            .await,
        )
    }

    #[tool(
        description = "List the storages of a node and which content types they accept",
        annotations(read_only_hint = true)
    )]
    async fn list_storages(&self, Parameters(p): Parameters<NodeParams>) -> String {
        render(
            async {
                let path = format!("/nodes/{}/storage", segment(&p.node)?);
                pretty(&self.c()?.get(&path, &[]).await?)
            }
            .await,
        )
    }

    #[tool(
        description = "List what a storage holds: ISO images, container templates, backups, disks",
        annotations(read_only_hint = true)
    )]
    async fn list_storage_content(&self, Parameters(p): Parameters<StorageContentParams>) -> String {
        render(
            async {
                let path = format!(
                    "/nodes/{}/storage/{}/content",
                    segment(&p.node)?,
                    segment(&p.storage)?
                );
                let mut q = Vec::new();
                opt(&mut q, "content", p.content);
                pretty(&self.c()?.get(&path, &q).await?)
            }
            .await,
        )
    }

    #[tool(
        description = "List the official container templates that can be downloaded with download_lxc_template",
        annotations(read_only_hint = true)
    )]
    async fn list_lxc_catalog(&self, Parameters(p): Parameters<NodeParams>) -> String {
        render(
            async {
                let path = format!("/nodes/{}/aplinfo", segment(&p.node)?);
                pretty(&self.c()?.get(&path, &[]).await?)
            }
            .await,
        )
    }

    #[tool(
        description = "Get the status of a task (download, backup, restore...) from its UPID",
        annotations(read_only_hint = true)
    )]
    async fn get_task_status(&self, Parameters(p): Parameters<TaskParams>) -> String {
        render(async { self.c()?.wait_task(&p.upid, 0).await }.await)
    }

    #[tool(
        description = "List the snapshots of a VM or container",
        annotations(read_only_hint = true)
    )]
    async fn list_snapshots(&self, Parameters(p): Parameters<GuestParams>) -> String {
        render(
            async {
                let path = format!("{}/snapshot", guest_path(&p.node, &p.kind, p.vmid)?);
                pretty(&self.c()?.get(&path, &[]).await?)
            }
            .await,
        )
    }

    // ---- Templates and images (need PROXMOX_ALLOW_WRITE=1) ----

    #[tool(
        description = "Download an ISO image from a URL into a storage. A checksum is mandatory and verified by Proxmox, certificates are verified",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn download_iso(&self, Parameters(p): Parameters<DownloadIsoParams>) -> String {
        render(
            async {
                let path = format!(
                    "/nodes/{}/storage/{}/download-url",
                    segment(&p.node)?,
                    segment(&p.storage)?
                );
                let algo = match p.checksum_algorithm {
                    ChecksumAlgorithm::Sha256 => "sha256",
                    ChecksumAlgorithm::Sha512 => "sha512",
                };
                if !p.checksum.chars().all(|c| c.is_ascii_hexdigit()) || p.checksum.is_empty() {
                    bail!("checksum must be a hexadecimal string");
                }
                if !p.filename.ends_with(".iso") {
                    bail!("filename must end in .iso");
                }
                let params = [
                    ("content", "iso".into()),
                    ("url", https_url(&p.url)?.to_owned()),
                    ("filename", segment(&p.filename)?.to_owned()),
                    ("checksum", p.checksum),
                    ("checksum-algorithm", algo.into()),
                    ("verify-certificates", "1".into()),
                ];
                self.c()?.write(Method::POST, &path, &params, 300).await
            }
            .await,
        )
    }

    #[tool(
        description = "Download an official container template (Debian, Ubuntu, Alpine...) into a storage",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn download_lxc_template(&self, Parameters(p): Parameters<DownloadLxcParams>) -> String {
        render(
            async {
                let path = format!("/nodes/{}/aplinfo", segment(&p.node)?);
                let params = [
                    ("storage", segment(&p.storage)?.to_owned()),
                    ("template", segment(&p.template)?.to_owned()),
                ];
                self.c()?.write(Method::POST, &path, &params, 300).await
            }
            .await,
        )
    }

    #[tool(
        description = "Convert a stopped VM or container into a read-only template that can be cloned. Irreversible: the guest can no longer be started",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn convert_to_template(&self, Parameters(p): Parameters<GuestParams>) -> String {
        render(
            async {
                let path = format!("{}/template", guest_path(&p.node, &p.kind, p.vmid)?);
                self.c()?.write(Method::POST, &path, &[], 120).await
            }
            .await,
        )
    }

    #[tool(
        description = "Create a new VM or container from a template (clone)",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn clone_guest(&self, Parameters(p): Parameters<CloneParams>) -> String {
        render(
            async {
                let path = format!("{}/clone", guest_path(&p.node, &p.kind, p.vmid)?);
                let name_key = match p.kind {
                    GuestKind::Qemu => "name",
                    GuestKind::Lxc => "hostname",
                };
                let params = [
                    ("newid", p.newid.to_string()),
                    (name_key, segment(&p.name)?.to_owned()),
                    ("full", flag(p.full.unwrap_or(true))),
                ];
                self.c()?.write(Method::POST, &path, &params, 300).await
            }
            .await,
        )
    }

    // ---- Create ----

    #[tool(
        description = "Create a container from a downloaded template. Unprivileged by default",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn create_container(&self, Parameters(p): Parameters<CreateContainerParams>) -> String {
        render(
            async {
                let path = format!("/nodes/{}/lxc", segment(&p.node)?);
                // These end up inside Proxmox property strings (`rootfs`, `net0`),
                // where a comma would add options of the caller's choosing.
                let bridge = segment(p.bridge.as_deref().unwrap_or("vmbr0"))?;
                let mut params = vec![
                    ("vmid", p.vmid.to_string()),
                    ("ostemplate", volid(&p.ostemplate)?.to_owned()),
                    ("hostname", segment(&p.hostname)?.to_owned()),
                    ("rootfs", format!("{}:{}", segment(&p.storage)?, p.disk_gb.unwrap_or(8))),
                    ("memory", p.memory_mb.unwrap_or(512).to_string()),
                    ("cores", p.cores.unwrap_or(1).to_string()),
                    ("net0", format!("name=eth0,bridge={bridge},ip=dhcp")),
                    ("unprivileged", flag(p.unprivileged.unwrap_or(true))),
                    ("start", flag(p.start.unwrap_or(false))),
                ];
                if p.nesting.unwrap_or(false) {
                    params.push(("features", "nesting=1".into()));
                }
                opt(&mut params, "ssh-public-keys", p.ssh_public_keys);
                self.c()?.write(Method::POST, &path, &params, 120).await
            }
            .await,
        )
    }

    #[tool(
        description = "Create a VM, optionally with an ISO attached to install from",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn create_vm(&self, Parameters(p): Parameters<CreateVmParams>) -> String {
        render(
            async {
                let path = format!("/nodes/{}/qemu", segment(&p.node)?);
                let bridge = segment(p.bridge.as_deref().unwrap_or("vmbr0"))?;
                let mut params = vec![
                    ("vmid", p.vmid.to_string()),
                    ("name", segment(&p.name)?.to_owned()),
                    ("memory", p.memory_mb.unwrap_or(2048).to_string()),
                    ("cores", p.cores.unwrap_or(2).to_string()),
                    ("scsihw", "virtio-scsi-single".into()),
                    ("scsi0", format!("{}:{}", segment(&p.storage)?, p.disk_gb.unwrap_or(32))),
                    ("net0", format!("virtio,bridge={bridge}")),
                ];
                if let Some(iso) = p.iso {
                    let iso = volid(&iso)?;
                    params.push(("ide2", format!("{iso},media=cdrom")));
                    params.push(("boot", "order=ide2;scsi0".into()));
                }
                self.c()?.write(Method::POST, &path, &params, 120).await
            }
            .await,
        )
    }

    #[tool(
        description = "Start, shut down, stop (hard power off) or reboot a VM or container",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn guest_power(&self, Parameters(p): Parameters<PowerParams>) -> String {
        render(
            async {
                let action = match p.action {
                    PowerAction::Start => "start",
                    PowerAction::Shutdown => "shutdown",
                    PowerAction::Stop => "stop",
                    PowerAction::Reboot => "reboot",
                };
                let path = format!("{}/status/{action}", guest_path(&p.node, &p.kind, p.vmid)?);
                self.c()?.write(Method::POST, &path, &[], 120).await
            }
            .await,
        )
    }

    // ---- Snapshots ----

    #[tool(
        description = "Take a snapshot of a VM or container",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn snapshot_create(&self, Parameters(p): Parameters<SnapshotCreateParams>) -> String {
        render(
            async {
                let path = format!("{}/snapshot", guest_path(&p.node, &p.kind, p.vmid)?);
                let mut params = vec![("snapname", segment(&p.snapname)?.to_owned())];
                opt(&mut params, "description", p.description);
                self.c()?.write(Method::POST, &path, &params, 120).await
            }
            .await,
        )
    }

    #[tool(
        description = "Roll a VM or container back to a snapshot. Everything done since the snapshot is lost",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn snapshot_rollback(&self, Parameters(p): Parameters<SnapshotParams>) -> String {
        render(
            async {
                let path = format!(
                    "{}/snapshot/{}/rollback",
                    guest_path(&p.node, &p.kind, p.vmid)?,
                    segment(&p.snapname)?
                );
                self.c()?.write(Method::POST, &path, &[], 120).await
            }
            .await,
        )
    }

    #[tool(
        description = "Delete a snapshot of a VM or container",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn snapshot_delete(&self, Parameters(p): Parameters<SnapshotParams>) -> String {
        render(
            async {
                let path = format!(
                    "{}/snapshot/{}",
                    guest_path(&p.node, &p.kind, p.vmid)?,
                    segment(&p.snapname)?
                );
                self.c()?.write(Method::DELETE, &path, &[], 120).await
            }
            .await,
        )
    }

    // ---- Backups ----

    #[tool(
        description = "Back up a VM or container to a storage (vzdump), compressed with zstd. Waits up to 10 minutes, then returns the task ID",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn backup_guest(&self, Parameters(p): Parameters<BackupParams>) -> String {
        render(
            async {
                let path = format!("/nodes/{}/vzdump", segment(&p.node)?);
                let mode = match p.mode.unwrap_or(BackupMode::Snapshot) {
                    BackupMode::Snapshot => "snapshot",
                    BackupMode::Suspend => "suspend",
                    BackupMode::Stop => "stop",
                };
                let params = [
                    ("vmid", p.vmid.to_string()),
                    ("storage", segment(&p.storage)?.to_owned()),
                    ("mode", mode.into()),
                    ("compress", "zstd".into()),
                ];
                self.c()?.write(Method::POST, &path, &params, 600).await
            }
            .await,
        )
    }

    #[tool(
        description = "Restore a backup into a guest ID. With force=true an existing guest with that ID is overwritten",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn restore_backup(&self, Parameters(p): Parameters<RestoreParams>) -> String {
        render(
            async {
                let path = format!(
                    "/nodes/{}/{}",
                    segment(&p.node)?,
                    p.kind.as_str()
                );
                let mut params = vec![
                    ("vmid", p.vmid.to_string()),
                    ("force", flag(p.force.unwrap_or(false))),
                ];
                let archive = volid(&p.archive)?.to_owned();
                match p.kind {
                    GuestKind::Lxc => {
                        params.push(("ostemplate", archive));
                        params.push(("restore", "1".into()));
                    }
                    GuestKind::Qemu => params.push(("archive", archive)),
                }
                if let Some(storage) = &p.storage {
                    segment(storage)?;
                }
                opt(&mut params, "storage", p.storage);
                self.c()?.write(Method::POST, &path, &params, 600).await
            }
            .await,
        )
    }

    // ---- Delete ----

    #[tool(
        description = "Permanently delete a stopped VM or container and its disks. confirm_vmid must repeat vmid",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn delete_guest(&self, Parameters(p): Parameters<DeleteGuestParams>) -> String {
        render(
            async {
                if p.confirm_vmid != p.vmid {
                    bail!("confirm_vmid ({}) does not match vmid ({})", p.confirm_vmid, p.vmid);
                }
                let path = guest_path(&p.node, &p.kind, p.vmid)?;
                let params = [("purge", flag(p.purge.unwrap_or(true)))];
                self.c()?.write(Method::DELETE, &path, &params, 120).await
            }
            .await,
        )
    }

    #[tool(
        description = "Permanently delete a stored file (backup, ISO or container template). confirm_volid must repeat volid",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn delete_volume(&self, Parameters(p): Parameters<DeleteVolumeParams>) -> String {
        render(
            async {
                if p.confirm_volid != p.volid {
                    bail!("confirm_volid does not match volid");
                }
                let storage = segment(&p.storage)?;
                let volid = volid(&p.volid)?;
                let Some(rest) = volid.strip_prefix(&format!("{storage}:")) else {
                    bail!("volid {volid:?} does not belong to storage {storage:?}");
                };
                // Guest disks go through delete_guest and its confirmation, not through here.
                if !["backup/", "iso/", "vztmpl/"].iter().any(|d| rest.starts_with(d)) {
                    bail!("only backups, ISO images and container templates can be deleted here");
                }
                let path = format!(
                    "/nodes/{}/storage/{storage}/content/{}",
                    segment(&p.node)?,
                    p.volid.replace('/', "%2F")
                );
                self.c()?.write(Method::DELETE, &path, &[], 60).await
            }
            .await,
        )
    }
}

#[tool_handler(
    name = "proxmox-mcp",
    instructions = "Manage a Proxmox VE host. Tools that modify anything only work when PROXMOX_ALLOW_WRITE=1. Typical rebuild after an incident: backup_guest (keep it for analysis), guest_power stop, delete_guest, clone_guest from the clean template, then restore only the data."
)]
impl ServerHandler for ProxmoxServer {}

#[tokio::main]
async fn main() -> Result<()> {
    let client = ProxmoxClient::from_env().map_err(|e| format!("{e:#}"));
    let service = ProxmoxServer {
        client: Arc::new(client),
    }
    .serve(stdio())
    .await?;
    service.waiting().await?;
    Ok(())
}
