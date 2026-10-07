use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Beat {
    pub seconds: f64,
    pub sample: u64,
    /// Sigmoid of the nearest 50 Hz beat-model logit, not a calibrated probability.
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub cache_key: String,
    pub detector: String,
    pub bpm: Option<f64>,
    pub confidence: f64,
    pub confidence_kind: String,
    pub interval_regularity: f64,
    pub beats: Vec<Beat>,
    pub downbeats: Vec<f64>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub index: u32,
    pub id: String,
    pub title: String,
    pub url: String,
    pub path: PathBuf,
    pub downloaded: bool,
    pub sha256: Option<String>,
    pub pcm_path: Option<PathBuf>,
    pub pcm_sha256: Option<String>,
    pub pcm_key: Option<String>,
    pub frames: Option<u64>,
    pub duration: Option<f64>,
    pub analysis: Option<Analysis>,
}
impl Track {
    pub fn new(index: u32, id: String, title: String, url: String, path: PathBuf) -> Self {
        Self {
            index,
            id,
            title,
            url,
            path,
            downloaded: false,
            sha256: None,
            pcm_path: None,
            pcm_sha256: None,
            pcm_key: None,
            frames: None,
            duration: None,
            analysis: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub outgoing_index: u32,
    pub incoming_index: u32,
    pub outgoing_beat_sample: u64,
    pub incoming_beat_sample: u64,
    pub outgoing_beat_seconds: f64,
    pub incoming_beat_seconds: f64,
    pub overlap_samples: u64,
    pub overlap_seconds: f64,
    pub outgoing_end_sample: u64,
    pub incoming_start_sample: u64,
    pub tail_trim_seconds: f64,
    pub intro_trim_seconds: f64,
    pub source: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub playlist_url: String,
    pub source: String,
    pub tools: BTreeMap<String, String>,
    pub config: BTreeMap<String, serde_json::Value>,
    pub tracks: Vec<Track>,
    pub transitions: Vec<Transition>,
    pub warnings: Vec<String>,
    pub last_error: Option<String>,
    pub output: Option<PathBuf>,
    pub output_sha256: Option<String>,
    pub output_plan_key: Option<String>,
}
impl Manifest {
    pub fn new(url: String, source: String, tracks: Vec<Track>) -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION,
            playlist_url: url,
            source,
            tools: BTreeMap::new(),
            config: BTreeMap::new(),
            tracks,
            transitions: vec![],
            warnings: vec![],
            last_error: None,
            output: None,
            output_sha256: None,
            output_plan_key: None,
        }
    }
    pub fn load(path: &Path) -> Result<Self> {
        let value: Self = serde_json::from_slice(&fs::read(path)?)
            .with_context(|| format!("Invalid manifest {}", path.display()))?;
        ensure!(
            value.schema_version == crate::SCHEMA_VERSION,
            "Unsupported manifest schema; use a new work directory"
        );
        Ok(value)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_json(path, self)
    }
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let partial = path.with_extension("json.partial");
    let mut file = File::create(&partial)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(partial, path)?;
    Ok(())
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
