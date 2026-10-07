pub mod analysis;
pub mod manifest;
pub mod models;
pub mod pipeline;
pub mod playlist;
pub mod process;
pub mod render;
pub mod transition;

pub const SAMPLE_RATE: u32 = 48_000;
pub const ANALYSIS_RATE: u32 = 22_050;
pub const SCHEMA_VERSION: u32 = 1;
