//! Blocking preparation only: no inference, network, providers or subprocess.
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use super::*;
use crate::ingest::import::inspect_media_with_cancel;
use crate::processing::local_moss::{LocalMossPlanSpec, MossWindowRequestSpec};

pub fn prepare_moss_source(
    app_root: &Path,
    store: &ProcessingStore,
    owner: &OwnerLease,
    claim: &MossPreparationClaim,
    cancel: &AtomicBool,
) -> Result<ProcessingLedger, ProcessingError> {
    let checkpoint = store.check_moss_preparation_claim(claim.recording_id(), owner, claim)?;
    let expected_root = store
        .root
        .parent()
        .ok_or_else(|| fail("unsafe_storage_layout"))?;
    if app_root != expected_root {
        return Err(fail("unsafe_storage_layout"));
    }
    files::check_cancel(cancel)?;
    let mut source = files::VerifiedSource::open(app_root, checkpoint.source(), cancel)?;
    let name = Path::new(&checkpoint.source().relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| fail("invalid_moss_preparation"))?;
    let extension = Path::new(name)
        .extension()
        .and_then(|name| name.to_str())
        .ok_or_else(|| fail("invalid_moss_preparation"))?;
    // The initial duration is only an ingest hint. Current decoded effective
    // frames are authoritative, including AAC priming and trailing padding.
    let media =
        inspect_media_with_cancel(source.file(), extension, || cancel.load(Ordering::Acquire))
            .map_err(|error| fail(error.code))?;
    source.verify(cancel)?;
    let decoded = echowall_local_audio::decode_source_with_cancel(
        source.cloned_file()?,
        name,
        media.duration_ms,
        || cancel.load(Ordering::Acquire),
    )
    .map_err(fail)?;
    source.verify(cancel)?;
    let pcm = quantize_pcm(&decoded, cancel)?;
    drop(decoded);
    let windows = moss::windows::plan_quiet_windows(
        pcm.len() as u64,
        |start, output| {
            let start =
                usize::try_from(start).map_err(|_| moss::ProtocolError("window_read_invalid"))?;
            let values = pcm
                .get(start..start + output.len())
                .ok_or(moss::ProtocolError("window_read_invalid"))?;
            output.copy_from_slice(values);
            Ok(())
        },
        || cancel.load(Ordering::Acquire),
    )
    .map_err(|error| fail(error.0))?;
    let mut requests = Vec::with_capacity(windows.windows.len());
    for window in windows.windows {
        files::check_cancel(cancel)?;
        store.check_moss_preparation_claim(claim.recording_id(), owner, claim)?;
        let prepared = files::publish_window(
            app_root,
            claim.recording_id(),
            claim.generation(),
            window.index,
            window.start_frame,
            &pcm[window.start_frame as usize..window.end_frame as usize],
            cancel,
        )?;
        store.record_moss_prepared_window(claim.recording_id(), owner, claim, prepared.clone())?;
        requests.push(MossWindowRequestSpec {
            index: prepared.index,
            start_frame: prepared.start_frame,
            end_frame: prepared.end_frame,
            request: moss::MossRequest {
                schema_version: moss::PROTOCOL_VERSION,
                recording_id: claim.recording_id(),
                runtime_id: moss::RUNTIME_ID.into(),
                model_id: moss::MODEL_ID.into(),
                model_revision: moss::MODEL_REVISION.into(),
                model_sha256: moss::MODEL_SHA256.into(),
                model_size_bytes: moss::MODEL_SIZE_BYTES,
                timing_policy: checkpoint.timing_policy().into(),
                audio_relative_path: prepared.relative_path,
                audio_sha256: prepared.sha256,
                audio_size_bytes: prepared.size_bytes,
                audio_duration_ms: (prepared.end_frame - prepared.start_frame).div_ceil(16),
                language: checkpoint.0.language.clone(),
            },
        });
    }
    let mut effective_source = checkpoint.source().clone();
    effective_source.duration_ms = media.duration_ms;
    let plan = LocalMossPlan::new_for_preparation(LocalMossPlanSpec {
        schema_version: checkpoint.plan_schema_version(),
        mapping_policy: checkpoint.0.mapping_policy.clone(),
        recording_id: claim.recording_id(),
        diarization_request: checkpoint.diarization_request(&effective_source),
        source: effective_source,
        pcm_sample_rate: windows.sample_rate,
        pcm_source_frames: windows.source_frames,
        pcm_quantization_policy: Some(PCM_QUANTIZATION_POLICY.into()),
        window_policy: windows.policy.into(),
        windows: requests,
    })
    .map_err(|error| fail(error.code))?;
    source.verify(cancel)?;
    files::check_cancel(cancel)?;
    store.check_moss_preparation_claim(claim.recording_id(), owner, claim)?;
    let artifacts = MossArtifacts::open(app_root, claim.recording_id(), claim.generation())
        .map_err(|error| fail(error.code()))?;
    let reference = artifacts
        .write(owner, ArtifactKind::Plan, plan.plan_bytes())
        .map_err(|error| fail(error.code()))?;
    files::check_cancel(cancel)?;
    store.complete_moss_preparation(claim.recording_id(), owner, claim, &plan, reference)
}

fn quantize_pcm(decoded: &[f32], cancel: &AtomicBool) -> Result<Vec<i16>, ProcessingError> {
    let mut pcm = Vec::with_capacity(decoded.len());
    for chunk in decoded.chunks(4096) {
        files::check_cancel(cancel)?;
        for sample in chunk {
            if !sample.is_finite() {
                return Err(fail("invalid_samples"));
            }
            // Rust's saturating float cast maps +1 to32767 and -1 to-32768.
            // Native S16 values are exactly representable and round-trip.
            pcm.push((sample.clamp(-1.0, 1.0) * 32768.0).round() as i16);
        }
    }
    Ok(pcm)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quantization_preserves_every_pcm16_value_and_is_explicit_at_fullscale() {
        let decoded: Vec<_> = (i16::MIN..=i16::MAX)
            .map(|sample| f32::from(sample) / 32768.0)
            .collect();
        let pcm = quantize_pcm(&decoded, &AtomicBool::new(false)).unwrap();
        assert!(pcm.into_iter().eq(i16::MIN..=i16::MAX));
        assert_eq!(
            quantize_pcm(&[-1.1, 1.0, 1.1], &AtomicBool::new(false)).unwrap(),
            [i16::MIN, i16::MAX, i16::MAX]
        );
        assert!(quantize_pcm(&[f32::NAN], &AtomicBool::new(false)).is_err());
        assert!(quantize_pcm(&[0.0], &AtomicBool::new(true)).is_err());
    }
}
