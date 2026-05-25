use claxon::FlacReader;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const TARGET_RATE: u32 = 48000;
const TARGET_BPS: u32 = 24;
const AUDIO_EXTS: &[&str] = &["flac", "wav", "mp3", "ogg", "m4a", "aiff", "aif"];

#[derive(Serialize, Deserialize)]
struct StemConfig {
    #[serde(default = "default_vocals")]
    vocals: String,
    #[serde(default = "default_bass")]
    bass: String,
    #[serde(default = "default_drums")]
    drums: String,
    #[serde(default = "default_other")]
    other: String,
}

fn default_vocals() -> String { "vocals".into() }
fn default_bass() -> String { "bass".into() }
fn default_drums() -> String { "drums".into() }
fn default_other() -> String { "other".into() }

impl Default for StemConfig {
    fn default() -> Self {
        Self {
            vocals: default_vocals(),
            bass: default_bass(),
            drums: default_drums(),
            other: default_other(),
        }
    }
}

static FFMPEG_OK: OnceLock<bool> = OnceLock::new();

fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("sp1-merge").join("config.toml"))
}

fn load_config() -> StemConfig {
    let path = match config_path() {
        Some(p) => p,
        None => return StemConfig::default(),
    };
    match fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => StemConfig::default(),
    }
}

fn save_config(config: &StemConfig) -> Result<(), Box<dyn Error>> {
    let path = config_path().ok_or("no config dir")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let toml_str = toml::to_string_pretty(config)?;
    fs::write(&path, toml_str)?;
    Ok(())
}

fn config_menu() -> Result<(), Box<dyn Error>> {
    let current = load_config();
    let labels = ["vocals", "bass", "drums", "other"];
    let defaults = [&current.vocals, &current.bass, &current.drums, &current.other];

    println!("config setup ->");
    let mut values: Vec<String> = Vec::new();

    for (label, default) in labels.iter().zip(defaults.iter()) {
        print!("{} stem name [{}]: ", label, default);
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim().to_string();
        values.push(if input.is_empty() {
            (*default).clone()
        } else {
            input
        });
    }

    let config = StemConfig {
        vocals: values[0].clone(),
        bass: values[1].clone(),
        drums: values[2].clone(),
        other: values[3].clone(),
    };
    save_config(&config)?;
    println!("saved to {}", config_path().unwrap_or_default().display());
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "config" {
        if let Err(e) = config_menu() {
            eprintln!("whoops: {}", e);
        }
        return;
    }

    let config = load_config();

    loop {
        let folder = match prompt_folder() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                eprintln!("whoops: {}", e);
                continue;
            }
        };

        match collect_targets(&folder, &config) {
            Ok(targets) => {
                for target in &targets {
                    if let Err(e) = process_one(target, &config) {
                        eprintln!("whoops: {}", e);
                    }
                }
            }
            Err(e) => eprintln!("whoops: {}", e),
        }
    }
}

fn collect_targets(folder: &Path, config: &StemConfig) -> Result<Vec<PathBuf>, String> {
    // first, check if the folder itself has all 4 stems
    let check = |path: &Path| -> bool {
        let names = [&config.vocals, &config.bass, &config.drums, &config.other];
        names.iter().all(|name| {
            AUDIO_EXTS.iter().any(|ext| path.join(format!("{}.{}", name, ext)).is_file())
        })
    };

    if check(folder) {
        return Ok(vec![folder.to_path_buf()]);
    }

    // scan one level of subdirectories
    let mut targets: Vec<PathBuf> = Vec::new();
    let entries = match fs::read_dir(folder) {
        Ok(e) => e,
        Err(e) => return Err(format!("can't read {}: {}", folder.display(), e)),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && check(&path) {
            targets.push(path);
        }
    }

    if targets.is_empty() {
        return Err(format!(
            "no stem folders found in {}. make sure each folder has: {}",
            folder.display(),
            [config.vocals.as_str(), config.bass.as_str(), config.drums.as_str(), config.other.as_str()].join(", ")
        ));
    }

    println!("found {} stem folder(s) in {}", targets.len(), folder.display());
    Ok(targets)
}

fn prompt_folder() -> Result<Option<PathBuf>, Box<dyn Error>> {
    print!("folder path (q to quit): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    if input.eq_ignore_ascii_case("q") {
        println!("quitting!");
        return Ok(None);
    }

    let path = PathBuf::from(input);

    if !path.is_dir() {
        return Err(format!("not a folder: {}", path.display()).into());
    }
    Ok(Some(path))
}

fn process_one(folder: &Path, config: &StemConfig) -> Result<(), Box<dyn Error>> {
    let paths = find_stems(folder, config)?;

    println!("processing stems...");

    let stems: Vec<Stem> = paths
        .iter()
        .map(|p| read_stem_with_cleanup(p))
        .collect::<Result<Vec<_>, _>>()?;

    let min_len = stems.iter().map(|s| s.left.len()).min().unwrap_or(0);
    if min_len == 0 {
        return Err("no audio found: check stems".into());
    }

    println!("turning {} samples per channel into 8 channel wav...", min_len);

    let folder_name = folder
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let output_path = folder.join(format!("{}.wav", folder_name));
    write_multichannel_wav(&output_path, &stems, min_len)?;

    println!("wrote: {}", output_path.display());
    Ok(())
}

struct Stem {
    left: Vec<f64>,
    right: Vec<f64>,
}

fn find_stems(folder: &Path, config: &StemConfig) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let names = [&config.vocals, &config.bass, &config.drums, &config.other];
    let labels = ["vocals", "bass", "drums", "other"];
    let mut results = Vec::with_capacity(4);

    for (name, label) in names.iter().zip(labels.iter()) {
        let mut found: Option<PathBuf> = None;
        for ext in AUDIO_EXTS {
            let candidate = folder.join(format!("{}.{}", name, ext));
            if candidate.is_file() {
                if found.is_some() {
                    return Err(format!(
                        "too many files for \"{}\" stem ({}.*) — clean up the folder",
                        label, name
                    )
                    .into());
                }
                found = Some(candidate);
            }
        }
        match found {
            Some(p) => results.push(p),
            None => {
                return Err(format!(
                    "no file found for \"{}\" stem (looking for {}.*)",
                    label, name
                )
                .into());
            }
        }
    }

    Ok(results)
}

fn convert_to_flac(input: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let parent = input.parent().unwrap_or(Path::new("."));
    let temp_path = parent.join(format!("{}.sp1tmp.flac", stem));

    let status = Command::new("ffmpeg")
        .args([
            "-y", "-i",
            &input.to_string_lossy(),
            "-c:a", "flac",
            "-compression_level", "8",
            &temp_path.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|_| "ffmpeg not found — install it to convert non-flac stems".to_string())?;

    if !status.success() {
        let _ = fs::remove_file(&temp_path);
        return Err("conversion blew up".into());
    }

    Ok(temp_path)
}

fn read_stem_with_cleanup(path: &Path) -> Result<Stem, Box<dyn Error>> {
    let actual_path = if path.extension().map_or(false, |e| e == "flac") {
        path.to_path_buf()
    } else {
        if FFMPEG_OK.get().is_none() {
            let ok = Command::new("ffmpeg")
                .arg("-version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok();
            FFMPEG_OK.set(ok).ok();
        }
        if !FFMPEG_OK.get().copied().unwrap_or(false) {
            return Err("ffmpeg not found — install it to convert non-flac stems".into());
        }
        convert_to_flac(path)?
    };

    let is_temp = actual_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .contains(".sp1tmp.");

    let result = read_flac_stem(&actual_path);

    if is_temp {
        let _ = fs::remove_file(&actual_path);
    }

    result
}

fn read_flac_stem(path: &Path) -> Result<Stem, Box<dyn Error>> {
    let mut reader = FlacReader::open(path)?;
    let info = reader.streaminfo();
    let src_rate = info.sample_rate;
    let src_bps = info.bits_per_sample;
    let channels = info.channels;

    if channels != 2 {
        return Err(format!(
            "{} has {} channels, expected 2. each stem must be stereo.",
            path.display(),
            channels
        )
        .into());
    }

    let max_val = (1i64 << (src_bps as i64 - 1)) as f64;

    let mut left = Vec::new();
    let mut right = Vec::new();
    {
        let mut samples_iter = reader.samples();
        while let (Some(Ok(l)), Some(Ok(r))) = (samples_iter.next(), samples_iter.next()) {
            left.push(l as f64 / max_val);
            right.push(r as f64 / max_val);
        }
    }

    if src_rate != TARGET_RATE {
        println!(
            "  resampling {} from {} hz to {} hz ({} samples)",
            path.file_name().unwrap_or_default().to_string_lossy(),
            src_rate,
            TARGET_RATE,
            left.len(),
        );
        let (l, r) = resample_stereo(&left, &right, src_rate, TARGET_RATE)?;
        return Ok(Stem { left: l, right: r });
    }

    Ok(Stem { left, right })
}

fn resample_stereo(
    left_in: &[f64],
    right_in: &[f64],
    input_rate: u32,
    output_rate: u32,
) -> Result<(Vec<f64>, Vec<f64>), Box<dyn Error>> {
    let ratio = output_rate as f64 / input_rate as f64;
    let input_len = left_in.len();

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    let mut resampler = SincFixedIn::<f64>::new(
        ratio,
        2.0,
        params,
        input_len,
        2,
    )?;

    let waves_in: [Vec<f64>; 2] = [left_in.to_vec(), right_in.to_vec()];
    let out = resampler.process(&waves_in, None)?;

    Ok((out[0].clone(), out[1].clone()))
}

fn write_multichannel_wav(
    path: &Path,
    stems: &[Stem],
    frames: usize,
) -> Result<(), Box<dyn Error>> {
    let file = fs::File::create(path)?;
    let mut writer = BufWriter::new(file);

    let data_size = (frames * 8 * 3) as u32;
    // RIFF file_size = everything after "RIFF" header:
    //   "WAVE"(4) + fmt_header(8) + fmt_extensible(40) + data_header(8) + data
    let file_size = 4 + 8 + 40 + 8 + data_size;

    // RIFF header
    writer.write_all(b"RIFF")?;
    writer.write_all(&file_size.to_le_bytes())?;
    writer.write_all(b"WAVE")?;

    // fmt chunk (WAVE_FORMAT_EXTENSIBLE)
    writer.write_all(b"fmt ")?;
    writer.write_all(&40u32.to_le_bytes())?;
    writer.write_all(&0xFFFEu16.to_le_bytes())?;
    writer.write_all(&8u16.to_le_bytes())?;
    writer.write_all(&TARGET_RATE.to_le_bytes())?;
    let byte_rate: u32 = TARGET_RATE * 8 * (TARGET_BPS / 8);
    writer.write_all(&byte_rate.to_le_bytes())?;
    let block_align: u16 = (8 * (TARGET_BPS / 8)) as u16;
    writer.write_all(&block_align.to_le_bytes())?;
    writer.write_all(&(TARGET_BPS as u16).to_le_bytes())?;
    writer.write_all(&22u16.to_le_bytes())?;
    writer.write_all(&(TARGET_BPS as u16).to_le_bytes())?;
    writer.write_all(&0u32.to_le_bytes())?;
    let pcm_guid: [u8; 16] = [
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00,
        0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
    ];
    writer.write_all(&pcm_guid)?;

    // data chunk
    writer.write_all(b"data")?;
    writer.write_all(&data_size.to_le_bytes())?;

    // Ch1: vocals L, Ch2: vocals R
    // Ch3: other L,   Ch4: other R
    // Ch5: bass L,    Ch6: bass R
    // Ch7: drums L,   Ch8: drums R
    let stem_order = [0usize, 3, 1, 2]; // vocals, other, bass, drums
    let max_24bit = (1i32 << 23) as f64;

    for i in 0..frames {
        for &stem_idx in &stem_order {
            let l = stems[stem_idx].left[i] * max_24bit;
            let r = stems[stem_idx].right[i] * max_24bit;
            let lo = (l.round() as i32).clamp(-(1i32 << 23), (1i32 << 23) - 1);
            let ro = (r.round() as i32).clamp(-(1i32 << 23), (1i32 << 23) - 1);
            writer.write_all(&lo.to_le_bytes()[..3])?;
            writer.write_all(&ro.to_le_bytes()[..3])?;
        }
    }

    writer.flush()?;
    Ok(())
}
