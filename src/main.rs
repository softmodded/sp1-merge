use claxon::FlacReader;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};
use std::error::Error;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

const TARGET_RATE: u32 = 48000;
const TARGET_BPS: u32 = 24;
const REQUIRED_FILES: &[&str] = &["vocals.flac", "bass.flac", "drums.flac", "other.flac"];

fn main() {
    loop {
        let folder = match prompt_folder() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                eprintln!("whoops: {}", e);
                continue;
            }
        };

        if let Err(e) = process_folder(&folder) {
            eprintln!("whoops: {}", e);
        }
    }
}

fn process_folder(folder: &Path) -> Result<(), Box<dyn Error>> {
    verify_files(folder)?;

    println!("processing stems...");

    let stems: Vec<Stem> = REQUIRED_FILES
        .iter()
        .map(|name| read_stereo_stem(&folder.join(name)))
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

fn verify_files(folder: &Path) -> Result<(), Box<dyn Error>> {
    for name in REQUIRED_FILES {
        let p = folder.join(name);
        if !p.is_file() {
            return Err(format!(
                "missing file: {}\nexpected all of: {}",
                p.display(),
                REQUIRED_FILES.join(", ")
            )
            .into());
        }
    }
    Ok(())
}

fn read_stereo_stem(path: &Path) -> Result<Stem, Box<dyn Error>> {
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

// gun to my head ask me what any of this means just pull the trigger
// i did not write this part lmao i hacked pieces together 
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
    writer.write_all(&40u32.to_le_bytes())?; // chunk size
    writer.write_all(&0xFFFEu16.to_le_bytes())?; // wFormatTag (extensible)
    writer.write_all(&8u16.to_le_bytes())?; // nChannels
    writer.write_all(&TARGET_RATE.to_le_bytes())?; // nSamplesPerSec
    let byte_rate: u32 = TARGET_RATE * 8 * (TARGET_BPS / 8);
    writer.write_all(&byte_rate.to_le_bytes())?; // nAvgBytesPerSec
    let block_align: u16 = (8 * (TARGET_BPS / 8)) as u16;
    writer.write_all(&block_align.to_le_bytes())?; // nBlockAlign
    writer.write_all(&(TARGET_BPS as u16).to_le_bytes())?; // wBitsPerSample
    writer.write_all(&22u16.to_le_bytes())?; // cbSize (extension)
    writer.write_all(&(TARGET_BPS as u16).to_le_bytes())?; // wValidBitsPerSample
    writer.write_all(&0u32.to_le_bytes())?; // dwChannelMask (no speaker assignment)
    // SubFormat GUID: KSDATAFORMAT_SUBTYPE_PCM
    // {00000001-0000-0010-8000-00AA00389B71}
    let pcm_guid: [u8; 16] = [
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00,
        0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
    ];
    writer.write_all(&pcm_guid)?;

    // data chunk
    writer.write_all(b"data")?;
    writer.write_all(&data_size.to_le_bytes())?;

    // Interleave 8 channels:
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
