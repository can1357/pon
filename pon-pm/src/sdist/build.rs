use crate::error::{Error, Result};

pub struct BuildRequest<'a> {
    pub normalized_name: &'a str,
    pub version: &'a str,
    pub filename: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildArtifact {
    pub wheel_filename: String,
}

pub trait SdistBuilder {
    fn build(&self, request: &BuildRequest<'_>) -> Result<BuildArtifact>;
}

pub struct CatalogSdistBuilder;

impl SdistBuilder for CatalogSdistBuilder {
    fn build(&self, request: &BuildRequest<'_>) -> Result<BuildArtifact> {
        match request.normalized_name {
            "flit-core" => Ok(BuildArtifact {
                wheel_filename: format!("flit_core-{}-py3-none-any.whl", request.version),
            }),
            other => Err(Error::UnsupportedArtifact(format!(
                "package `{other}` is not in the deterministic sdist catalog"
            ))),
        }
    }
}
