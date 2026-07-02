#![doc = "Package-management dependency spine for Pon projects."]

pub mod cli;
pub mod editable;
pub mod env;
pub mod error;
pub mod index;
pub mod install;
pub mod local;
pub mod lock;
pub mod marker;
pub mod metadata;
pub mod names;
pub mod native;
pub mod pyproject;
pub mod requirement;
pub mod requirements;
pub mod resolve;
pub mod sdist;
pub mod vcs;
pub mod wheel;

pub use error::{Error, Result};
