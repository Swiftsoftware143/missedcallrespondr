pub mod boundary;
pub mod handlers;
pub mod middleware;
pub mod models;
pub mod route_policy;
/// The ONE writer of a self-serve account (kanban t_1d08bd9a): the public `register` handler and
/// the fleet-internal `provision_handler::provision_free_account` both mint through it, so the
/// account a FunnelSwift tag creates is byte-identical to the one the signup form creates.
pub mod signup;
