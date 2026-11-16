//! The repository handle's implementation, split along its topic seams.

mod checkout;
mod history;
mod index;
mod journal;
mod managed;
mod object;
mod observe;
mod publish;
mod read;
mod review;
#[cfg(test)]
mod store_index_batching_tests;
