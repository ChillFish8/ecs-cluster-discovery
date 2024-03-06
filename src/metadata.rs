//! ECS Container Metadata Handling and Extraction

use std::io;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::Duration;

use tracing::warn;

#[derive(Debug, Clone, serde_derive::Deserialize)]
#[serde(rename_all = "PascalCase")]
/// ECS Container Metadata
///
/// This information is automatically injected in all ECS containers
/// and can be found via the `$ECS_CONTAINER_METADATA_FILE` env var.
///
/// See https://docs.aws.amazon.com/AmazonECS/latest/developerguide/container-metadata.html
/// and https://docs.aws.amazon.com/AmazonECS/latest/developerguide/metadata-file-format.html
/// for more information.
pub struct ContainerMetadata {
    /// The name of the cluster that the container's task is running on.
    pub cluster: String,
    #[serde(rename = "ContainerInstanceARN")]
    /// The full Amazon Resource Name (ARN) of the host container instance.
    pub cluster_instance_arn: String,
    #[serde(rename = "TaskARN")]
    /// The full Amazon Resource Name (ARN) of the task that the container belongs to.
    pub tark_arn: String,
    /// The name of the task definition family the container is using.
    pub task_definition_family: String,
    /// The task definition revision the container is using.
    pub task_definition_revision: usize,
    #[serde(rename = "ContainerID")]
    /// The Docker container ID (and not the Amazon ECS container ID) for the container.
    pub container_id: String,
    /// The container name from the Amazon ECS task definition for the container.
    pub container_name: String,
    /// The timestamp in which the container was created.
    pub created_at: String,
    /// The timestamp in which the container was started.
    pub started_at: String,
    /// The container name that the Docker daemon uses for the container
    /// (for example, the name that shows up in docker ps command output).
    pub docker_container_name: String,
    #[serde(rename = "ImageID")]
    /// The SHA digest for the Docker image used to start the container.
    pub image_id: String,
    /// The image name and tag for the Docker image used to start the container.
    pub image_name: String,
    /// Any port mappings associated with the container.
    pub port_mappings: Vec<PortMapping>,
    /// The network mode and IP address for the container.
    pub networks: Vec<Network>,
    /// This should always be `READY` as the library waits until all
    /// info is available.
    pub metadata_file_status: String,
    /// The Availability Zone the host container instance resides in.
    pub availability_zone: String,
    #[serde(rename = "HostPrivateIPv4Address")]
    /// The private IP address for the task the container belongs to.
    pub host_private_ipv4_address: Ipv4Addr,
    #[serde(rename = "HostPublicIPv4Address")]
    /// The public IP address for the task the container belongs to.
    pub host_public_ipv4_address: Ipv4Addr,
}

#[derive(Debug, Clone, serde_derive::Deserialize)]
#[serde(rename_all = "PascalCase")]
/// Network port mapping for the container.
pub struct PortMapping {
    /// The port on the container that is exposed.
    pub container_port: u16,
    /// The port on the host container instance that is exposed.
    pub host_port: u16,
    /// The bind IP address that is assigned to the container by Docker.
    /// This IP address is only applied with the bridge network mode, and it is only
    /// accessible from the container instance.
    pub bind_ip: IpAddr,
    /// The network protocol used for the port mapping.
    pub protocol: String,
}

#[derive(Debug, Clone, serde_derive::Deserialize)]
#[serde(rename_all = "PascalCase")]
/// The task definition network config.
pub struct Network {
    /// The network mode for the task to which the container belongs.
    pub network_mode: String,
    #[serde(rename = "IPv4Addresses")]
    /// The IP addresses associated with the container.
    ///
    /// NOTE:
    /// If your task is using the awsvpc network mode, the IP address of the container will
    /// not be returned. In this case, you can retrieve the IP address by
    /// reading the /etc/hosts file.
    pub ipv4_addresses: Vec<Ipv4Addr>,
}

/// Returns if the container / program is running within ECS.
pub fn is_on_ecs() -> bool {
    match std::env::var("ECS_CONTAINER_METADATA_FILE") {
        Err(_) => false,
        Ok(path) => {
            let path: &Path = path.as_ref();
            path.exists()
        },
    }
}

/// Read the ECS container metadata file.
///
/// This gives some information like cluster name, private IPV4 and public IPV4.
///
/// If the file cannot be read within 1.5s and 3 attempts, a [io::Error] is returned.
pub async fn read_ecs_metadata_file() -> Result<ContainerMetadata, io::Error> {
    let path = std::env::var("ECS_CONTAINER_METADATA_FILE")
        .map_err(|_| io::Error::new(ErrorKind::NotFound, "No metadata file present"))?;

    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(500)).await;

        let content = tokio::fs::read(&path).await?;

        let metadata = serde_json::from_slice::<ContainerMetadata>(&content);
        if let Ok(metadata) = metadata {
            return Ok(metadata);
        }

        warn!("Failed to read initial metadata payload, waiting until file is READY");
    }

    Err(io::Error::new(
        ErrorKind::InvalidData,
        "Malformed metadata file present",
    ))
}
