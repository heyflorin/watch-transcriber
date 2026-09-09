//! App-owned policy composition over already adapted, validated MOSS intervals.
//! Physical request-window count is the only selector; text and anchor content
//! never select a policy. Graph evidence retains its original JSON shape.

use super::{error, speakers, LocalMossError, LocalMossPlan, COMPOSED_MAPPING_POLICY};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum MossMappingEvidence {
    Graph(speakers::MappingResult),
    NativeSlots(NativeSlotMappingEvidence),
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeSlotMappingEvidence {
    pub policy: &'static str,
    pub assignment_source: &'static str,
    pub assignments: Vec<Option<String>>,
    pub windows: Vec<speakers::Window>,
    pub input_unknown_segments: usize,
    pub output_unknown_segments: usize,
    pub new_unknown_segments: usize,
}

impl MossMappingEvidence {
    pub fn assignments(&self) -> &[Option<String>] {
        match self {
            Self::Graph(mapping) => &mapping.assignments,
            Self::NativeSlots(mapping) => &mapping.assignments,
        }
    }

    pub fn output_unknown_segments(&self) -> usize {
        match self {
            Self::Graph(mapping) => mapping.output_unknown_segments,
            Self::NativeSlots(mapping) => mapping.output_unknown_segments,
        }
    }
}

pub(super) fn reconcile_with_cancel(
    plan: &LocalMossPlan,
    timing: &[speakers::TimingSegment],
    anchors: &[speakers::TimingSegment],
    windows: &[speakers::Window],
    cancelled: &mut impl FnMut() -> bool,
) -> Result<MossMappingEvidence, LocalMossError> {
    if plan.mapping_policy() == COMPOSED_MAPPING_POLICY && plan.windows().len() == 1 {
        let mut assignments = Vec::with_capacity(timing.len());
        let mut unknown = 0;
        for segment in timing {
            if cancelled() {
                return Err(error("mapping_cancelled"));
            }
            unknown += usize::from(segment.speaker.is_none());
            assignments.push(segment.speaker.clone());
        }
        return Ok(MossMappingEvidence::NativeSlots(
            NativeSlotMappingEvidence {
                policy: COMPOSED_MAPPING_POLICY,
                assignment_source: "adapted_joint_moss_slots",
                assignments,
                windows: windows.to_vec(),
                input_unknown_segments: unknown,
                output_unknown_segments: unknown,
                new_unknown_segments: 0,
            },
        ));
    }
    speakers::reconcile_with_cancel(
        timing,
        anchors,
        windows,
        plan.timeline_duration_ms(),
        cancelled,
    )
    .map(MossMappingEvidence::Graph)
    .map_err(|failure| error(failure.code()))
}
