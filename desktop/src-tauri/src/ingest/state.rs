use serde::{Deserialize, Serialize};

/// Durable local/client job states shared with `RecordingEnvelope` v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Draft,
    Recording,
    Importing,
    Finalizing,
    Ready,
    Uploading,
    Queued,
    Transcribing,
    Submitting,
    Summarizing,
    Publishing,
    Complete,
    Interrupted,
    Offline,
    UploadFailed,
    ProviderFailed,
    PublishFailed,
    PublishAmbiguous,
    SubmitAmbiguous,
    Recovered,
    CanceledBeforeUpload,
    CanceledAfterUpload,
}

impl JobState {
    pub const ALL: [Self; 22] = [
        Self::Draft,
        Self::Recording,
        Self::Importing,
        Self::Finalizing,
        Self::Ready,
        Self::Uploading,
        Self::Queued,
        Self::Transcribing,
        Self::Submitting,
        Self::Summarizing,
        Self::Publishing,
        Self::Complete,
        Self::Interrupted,
        Self::Offline,
        Self::UploadFailed,
        Self::ProviderFailed,
        Self::PublishFailed,
        Self::PublishAmbiguous,
        Self::SubmitAmbiguous,
        Self::Recovered,
        Self::CanceledBeforeUpload,
        Self::CanceledAfterUpload,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Recording => "recording",
            Self::Importing => "importing",
            Self::Finalizing => "finalizing",
            Self::Ready => "ready",
            Self::Uploading => "uploading",
            Self::Queued => "queued",
            Self::Transcribing => "transcribing",
            Self::Submitting => "submitting",
            Self::Summarizing => "summarizing",
            Self::Publishing => "publishing",
            Self::Complete => "complete",
            Self::Interrupted => "interrupted",
            Self::Offline => "offline",
            Self::UploadFailed => "upload_failed",
            Self::ProviderFailed => "provider_failed",
            Self::PublishFailed => "publish_failed",
            Self::PublishAmbiguous => "publish_ambiguous",
            Self::SubmitAmbiguous => "submit_ambiguous",
            Self::Recovered => "recovered",
            Self::CanceledBeforeUpload => "canceled_before_upload",
            Self::CanceledAfterUpload => "canceled_after_upload",
        }
    }

    /// The exact legal transition graph used by the embedded Rust engine.
    pub const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Draft => matches!(
                next,
                Self::Recording | Self::Importing | Self::Finalizing | Self::CanceledBeforeUpload
            ),
            Self::Recording | Self::Importing => matches!(
                next,
                Self::Finalizing | Self::Interrupted | Self::CanceledBeforeUpload
            ),
            Self::Finalizing => matches!(
                next,
                Self::Ready | Self::Interrupted | Self::CanceledBeforeUpload
            ),
            Self::Ready => matches!(
                next,
                Self::Uploading | Self::Queued | Self::CanceledBeforeUpload
            ),
            Self::Uploading => matches!(
                next,
                Self::Queued | Self::Offline | Self::UploadFailed | Self::CanceledAfterUpload
            ),
            Self::Queued => {
                matches!(next, Self::Transcribing | Self::CanceledAfterUpload)
            }
            Self::Transcribing => matches!(
                next,
                Self::Submitting
                    | Self::Summarizing
                    | Self::ProviderFailed
                    | Self::CanceledAfterUpload
            ),
            Self::Submitting => matches!(
                next,
                Self::Transcribing
                    | Self::ProviderFailed
                    | Self::SubmitAmbiguous
                    | Self::CanceledAfterUpload
            ),
            Self::Summarizing => matches!(
                next,
                Self::Publishing | Self::ProviderFailed | Self::CanceledAfterUpload
            ),
            Self::Publishing => matches!(
                next,
                Self::Complete
                    | Self::PublishFailed
                    | Self::PublishAmbiguous
                    | Self::CanceledAfterUpload
            ),
            Self::Interrupted => matches!(
                next,
                Self::Recovered | Self::CanceledBeforeUpload | Self::CanceledAfterUpload
            ),
            Self::Offline => matches!(
                next,
                Self::Uploading | Self::Queued | Self::Recovered | Self::CanceledAfterUpload
            ),
            Self::UploadFailed => {
                matches!(next, Self::Uploading | Self::CanceledAfterUpload)
            }
            Self::ProviderFailed => matches!(
                next,
                Self::Transcribing | Self::Summarizing | Self::CanceledAfterUpload
            ),
            Self::PublishFailed => {
                matches!(next, Self::Publishing | Self::CanceledAfterUpload)
            }
            Self::PublishAmbiguous => matches!(next, Self::CanceledAfterUpload),
            Self::Recovered => matches!(
                next,
                Self::Ready
                    | Self::Uploading
                    | Self::Queued
                    | Self::Transcribing
                    | Self::Summarizing
                    | Self::Publishing
            ),
            Self::SubmitAmbiguous => matches!(
                next,
                Self::Transcribing | Self::ProviderFailed | Self::CanceledAfterUpload
            ),
            Self::Complete | Self::CanceledBeforeUpload | Self::CanceledAfterUpload => false,
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::CanceledBeforeUpload | Self::CanceledAfterUpload
        )
    }

    pub fn transition_to(self, next: Self) -> Result<Self, InvalidJobTransition> {
        require_transition(self, next)?;
        Ok(next)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidJobTransition {
    pub from: JobState,
    pub to: JobState,
}

impl std::fmt::Display for InvalidJobTransition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "job transition {:?} -> {:?} is not allowed",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidJobTransition {}

pub fn require_transition(from: JobState, to: JobState) -> Result<(), InvalidJobTransition> {
    if from.can_transition_to(to) {
        Ok(())
    } else {
        Err(InvalidJobTransition { from, to })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_forward_and_recovery_edges() {
        assert!(JobState::Draft.can_transition_to(JobState::Recording));
        assert!(JobState::Ready.can_transition_to(JobState::Uploading));
        assert!(JobState::Uploading.can_transition_to(JobState::Offline));
        assert!(JobState::ProviderFailed.can_transition_to(JobState::Summarizing));
        assert!(JobState::Transcribing.can_transition_to(JobState::Submitting));
        assert!(JobState::Submitting.can_transition_to(JobState::SubmitAmbiguous));
        assert!(JobState::Publishing.can_transition_to(JobState::PublishAmbiguous));
        assert!(JobState::PublishAmbiguous.can_transition_to(JobState::CanceledAfterUpload));
        assert!(JobState::Recovered.can_transition_to(JobState::Publishing));
        assert!(JobState::SubmitAmbiguous.can_transition_to(JobState::Transcribing));
        assert_eq!(
            JobState::Ready.transition_to(JobState::Uploading).unwrap(),
            JobState::Uploading
        );
    }

    #[test]
    fn rejects_skips_self_edges_and_terminal_restarts() {
        assert!(require_transition(JobState::Draft, JobState::Complete).is_err());
        assert!(require_transition(JobState::Ready, JobState::Ready).is_err());
        assert!(require_transition(JobState::Complete, JobState::Ready).is_err());
        assert!(require_transition(JobState::CanceledAfterUpload, JobState::Uploading).is_err());
        assert!(JobState::Complete.is_terminal());
        assert!(!JobState::SubmitAmbiguous.is_terminal());
    }
}
