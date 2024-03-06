# ECS Cluster Discovery

A smart-ish way of ECS tasks being able to discover their peers within their ECS cluster and VPC.

This system requires no interaction with AWS Cloud Map or services manually registering themselves, instead the
system does the following:

- User provides the VPC subnet.
- Extract ECS container metadata file to discover what cluster it is in.
- The system filters all active tasks in the cluster and that match the given service name filter (if applicable)
- For each selected service the currently running ECS tasks are selected and the first `Elastic network interface`
  is selected that is within the specified VPC ip.
- Upto 100 peers per service are collected and then a subset of peers is returned following a weighted selection
  process.

### Features

- Automatic detection of ECS environment.
- ECS metadata exposed and retrievable.
- Getting peers by cluster name and filter by service name.
- Weighting selected peers based on service bias (i.e. service `A` should have more peers selected than service `B`.)
- Retrieval of both internal DNS name and VPC IPv4 address.
- Automatic subnet validation


### What this library doesn't do

- Detect what port the peer servers are listening on
- Do multi-VPC handling
- Cross-cluster discovery (By this point AWS Cloud Map will be easier)

### Required AWS Permissions

- `ecs:ListTasks`
- `ecs:ListServices`
- `ecs:DescribeTasks`

These permissions must be granted at least to the cluster
the host is running on.