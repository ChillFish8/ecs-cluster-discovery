pub mod metadata;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Display;
use std::io;
use std::net::{IpAddr, Ipv4Addr};

use aws_config::BehaviorVersion;
use aws_sdk_ecs::types::DesiredStatus;
use aws_sdk_ecs::Client;
use tracing::warn;

type ServiceWeights = BTreeMap<String, f32>;
type PeersByService = BTreeMap<String, HashSet<String>>;

#[derive(Debug, thiserror::Error)]
#[error("Failed to get peers due to error: {0}")]
/// A error encountered when attempting to retrieve cluster peers.
pub struct GetPeersError(pub String);

impl From<io::Error> for GetPeersError {
    fn from(value: io::Error) -> Self {
        Self(value.to_string())
    }
}

/// Retrieve a set of peers from the instance's ECS cluster.
///
/// Peers can be a part of the FARGATE managed compute system, or
/// can also be a part of a EC2 container instance.
///
/// This builder provided some level of filtering and granularity
/// of what peers are selected, including filtering by service name prefix
/// and limiting the number of peers returned based on some weight.
pub struct GetPeersBuilder {
    subnet: ipnet::IpNet,
    services: Vec<String>,
    max_peers: usize,
    return_dns_name: bool,
    one_peer_per_service: bool,
    service_weights: ServiceWeights,
}

impl GetPeersBuilder {
    /// Create a new [GetPeersBuilder] instance.
    ///
    /// This takes the VPC subnet of the cluster in order to
    /// select the peers the system can correctly talk to.
    pub fn new(subnet: ipnet::IpNet) -> Self {
        Self {
            subnet,
            services: Vec::new(),
            max_peers: 10,
            return_dns_name: false,
            one_peer_per_service: false,
            service_weights: BTreeMap::new(),
        }
    }

    /// Add a new service filter to the builder.
    ///
    /// When one or more service filters are specified, only tasks
    /// within those services will be selected.
    ///
    /// NOTE: This filter is always a _exact_ match.
    pub fn with_service_filter(mut self, service: impl Display) -> Self {
        let service = service.to_string().to_lowercase();        
        self.services.push(service);
        self
    }

    /// Set the maximum number of peers to return.
    ///
    /// By default, the system will attempt to evenly select a number of peers
    /// from each matching service within the cluster unless custom weighting is
    /// provided via [GetPeersBuilder::with_service_weight].
    pub fn with_max_peers(mut self, max_peers: usize) -> Self {
        assert!(max_peers > 0, "Max peers cannot be 0");
        assert!(max_peers <= 100, "No more than 100 peers can be returned");
        self.max_peers = max_peers;
        self
    }

    /// Set a custom service weight.
    ///
    /// This weight is used when selecting the top K peers to return, services with a
    /// larger weight will have a higher number of peers returned for that specific
    /// instance.
    ///
    /// This method can be useful in large clusters where you might not want all peers,
    /// but still want to prioritise certain services over others.
    pub fn with_service_weight(mut self, service: impl Display, weight: f32) -> Self {
        assert!(weight.is_finite(), "Weight must be finite");
        let service = service.to_string().to_lowercase();
        self.service_weights.insert(service, weight);
        self
    }

    /// Return the peer addresses in the form of the DNS name rather than Ipv4 address.
    ///
    /// DNS names are typically in the form of `some-instance.ec2.local`.
    pub fn with_return_dns_name(mut self) -> Self {
        self.return_dns_name = true;
        self
    }

    /// Return at least 1 peer from each service regardless of the weight.
    ///
    /// This can help protect against accidental node isolation.
    pub fn with_at_least_one_peer_per_service(mut self) -> Self {
        self.one_peer_per_service = true;
        self
    }

    /// Execute the request to get the cluster peer addresses.
    ///
    /// This uses the auto-loaded AWS SDK config which may not be desired
    /// if you need a custom client, please see [GetPeersBuilder::execute_with_sdk_config]
    /// if you need more control over the SDK client.
    pub async fn execute(self) -> Result<Vec<String>, GetPeersError> {
        let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
        self.execute_with_sdk_config(&config).await
    }

    /// Execute the request to get the cluster peer addresses.
    ///
    /// This method takes a custom AWS SDK config which can be configured to meet
    /// your general requirements.
    pub async fn execute_with_sdk_config(
        self,
        config: &aws_config::SdkConfig,
    ) -> Result<Vec<String>, GetPeersError> {
        let client = Client::new(config);
        let ecs_metadata = metadata::read_ecs_metadata_file().await?;
        validate_host_within_subnet(
            &self.subnet,
            ecs_metadata.host_private_ipv4_address,
        )?;

        let mut peers_by_service = PeersByService::new();

        self.select_peers(&client, &ecs_metadata.cluster, &mut peers_by_service)
            .await?;

        let peers = filter_peers_with_bias(
            peers_by_service,
            self.service_weights,
            self.max_peers,
            self.one_peer_per_service,
        );

        Ok(peers)
    }

    async fn select_peers(
        &self,
        client: &Client,
        cluster_name: &str,
        peers_by_service: &mut PeersByService,
    ) -> Result<(), GetPeersError> {
        let tasks_by_service =
            fetch_tasks_for_services(client, cluster_name, self.services.clone())
                .await?;

        for (service, tasks) in tasks_by_service {
            let peers = fetch_peers_from_tasks(
                client,
                cluster_name,
                self.subnet,
                self.return_dns_name,
                tasks,
            )
            .await?;

            peers_by_service
                .entry(service)
                .or_default()
                .extend(peers.into_iter().take(self.max_peers));
        }

        Ok(())
    }
}

fn validate_host_within_subnet(
    subnet: &ipnet::IpNet,
    host_private_ipv4: Ipv4Addr,
) -> Result<(), GetPeersError> {
    let private_addr = IpAddr::V4(host_private_ipv4);
    if !subnet.contains(&private_addr) {
        Err(GetPeersError(format!(
            "Host address {} is not within specified VPC subnet ({}), no peers can be contacted",
            host_private_ipv4,
            subnet,
        )))
    } else {
        Ok(())
    }
}

async fn fetch_peers_from_tasks(
    client: &Client,
    cluster_name: &str,
    subnet: ipnet::IpNet,
    return_dns_name: bool,
    tasks: Vec<String>,
) -> Result<Vec<String>, GetPeersError> {
    let task_descriptions = client
        .describe_tasks()
        .cluster(cluster_name)
        .set_tasks(Some(tasks))
        .send()
        .await
        .map_err(|e| GetPeersError(e.to_string()))?;

    let mut service_addresses = Vec::new();
    for task in task_descriptions.tasks() {
        let addresses = task
            .attachments()
            .iter()
            .filter(|attachment| {
                matches!(attachment.r#type(), Some("ElasticNetworkInterface"))
            })
            .map(|attachment| {
                attachment
                    .details
                    .clone()
                    .unwrap()
                    .into_iter()
                    .filter_map(|el| Some((el.name?, el.value?)))
                    .collect::<BTreeMap<String, String>>()
            })
            .filter_map(|mut interface| {
                let mut address = interface.remove("privateIPv4Address")?;
                let ip = address.parse::<Ipv4Addr>().ok()?;

                // Validate the given IP lies within the defined subnet.
                if !subnet.contains(&IpAddr::V4(ip)) {
                    return None;
                }

                if return_dns_name {
                    address = interface.remove("privateDnsName")?;
                }

                Some(address)
            });

        service_addresses.extend(addresses);
    }

    Ok(service_addresses)
}

async fn fetch_tasks_for_services(
    client: &Client,
    cluster_name: &str,
    mut services: Vec<String>,
) -> Result<BTreeMap<String, Vec<String>>, GetPeersError> {
    if services.is_empty() {
        services = discover_service_names(client, cluster_name).await?;
    }

    let mut tasks_by_service = BTreeMap::new();
    for service in services {
        let service_tasks = client
            .list_tasks()
            .cluster(cluster_name)
            .desired_status(DesiredStatus::Running)
            .service_name(&service)
            .send()
            .await
            .map_err(|e| GetPeersError(e.to_string()))?;

        tasks_by_service.insert(service, service_tasks.task_arns.unwrap_or_default());
    }

    Ok(tasks_by_service)
}

async fn discover_service_names(
    client: &Client,
    cluster_name: &str,
) -> Result<Vec<String>, GetPeersError> {
    let cluster_services = client
        .list_services()
        .cluster(cluster_name)
        .max_results(100)
        .send()
        .await
        .map_err(|e| GetPeersError(e.to_string()))?;

    // arn:aws:ecs:us-east-1:xxxxxxx:service/ecs-cluster/service-name -> service-name
    let mut service_names = Vec::new();
    for arn in cluster_services.service_arns.unwrap_or_default() {
        match arn.rsplit_once('/') {
            None => continue,
            Some((_, service_name)) => {
                service_names.push(service_name.to_string());
            },
        }
    }

    Ok(service_names)
}

fn filter_peers_with_bias(
    peers_by_service: PeersByService,
    weights: ServiceWeights,
    max_peers: usize,
    at_least_one: bool,
) -> Vec<String> {
    if peers_by_service.is_empty() {
        return Vec::new();
    }

    let mut normalized_weights = normalize_weights(weights);
    add_default_service_weights(&peers_by_service, &mut normalized_weights);
    
    let max_peers_by_service =
        get_max_peers_per_service(normalized_weights, max_peers, at_least_one);

    let mut selected_peers = Vec::new();
    for (service_name, peers) in peers_by_service {
        let take_n = max_peers_by_service
            .get(&service_name)
            .copied()
            .unwrap_or(0);

        selected_peers.extend(peers.into_iter().take(take_n));
    }

    selected_peers
}

fn add_default_service_weights(
    peers_by_service: &PeersByService,
    weights: &mut ServiceWeights,
) {
    let total_services = peers_by_service.len();
    let total_pre_weighted_services = weights.len();
    
    weights.retain(|service, _| peers_by_service.contains_key(service));
    
    let total_non_weighted_services = total_services - total_pre_weighted_services;
    let default_weight = total_non_weighted_services as f32 / total_services as f32;
    
    for service in peers_by_service.keys() {
        if weights.contains_key(service) {
            continue
        }
        
        weights.insert(service.clone(), default_weight);
    }
}

fn normalize_weights(weights: ServiceWeights) -> ServiceWeights {
    let total = weights.values().sum::<f32>();
        
    weights
        .into_iter()
        .map(|(service, relative_weight)| (service, relative_weight / total))
        .collect()
}

fn get_max_peers_per_service(
    weights: ServiceWeights,
    max_peers: usize,
    at_least_one: bool,
) -> BTreeMap<String, usize> {
    weights
        .into_iter()
        .map(|(service, weight)| {
            let mut n = (max_peers as f32 / weight) as usize;

            if n == 0 && at_least_one {
                n = 1;
            } else if n == 0 {
                warn!(
                    service = service,
                    weight = weight,
                    "Service has no peers selected due to weighting"
                );
            };

            (service, n)
        })
        .collect()
}
