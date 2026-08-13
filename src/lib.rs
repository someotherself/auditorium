pub mod chain;
pub mod device;
pub mod device_builder;
pub mod host;
pub mod sources;
pub mod store_ops;
pub mod tracked_source;

pub use maudio::audio::{performance, sample_rate, wave_shape};
pub use maudio::context;
pub use maudio::device::{device_id, device_info, device_type};

pub type HostResult<T> = Result<T, AuditoriumError>;

#[derive(Debug, Eq, PartialEq, Clone)]
#[non_exhaustive]
pub enum AuditoriumError {
    Maudio(maudio::MaudioError),
    ChannelSend,
    ChannelRecv,
    HostShutdown,
    ThreadJoin,
    InvalidDevice,
    EndOfChain,
    DanglingChain,
    IoError { err: std::io::ErrorKind },
    Other { msg: String },
}

impl std::fmt::Display for AuditoriumError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChannelSend => write!(f, "failed to sned to the host channel"),
            Self::ChannelRecv => write!(f, "failed to receive from the host channel"),
            Self::ThreadJoin => write!(f, "failed to join host thread"),
            Self::HostShutdown => write!(f, "the host has been shutdown"),
            Self::InvalidDevice => write!(f, "engine not found, or wrong ID"),
            Self::Maudio(e) => write!(f, "maudio error: {}", e),
            Self::IoError { err } => write!(f, "IO error: {err}"),
            Self::EndOfChain => write!(f, "Invalid dsp element, or reached end of dsp chain"),
            Self::DanglingChain => write!(f, "Dsp chain does not have a source to attach to"),
            Self::Other { msg } => write!(f, "Error: {}", msg),
        }
    }
}

impl From<std::io::Error> for AuditoriumError {
    fn from(value: std::io::Error) -> Self {
        Self::IoError { err: value.kind() }
    }
}

impl std::error::Error for AuditoriumError {}

impl From<maudio::MaudioError> for AuditoriumError {
    fn from(err: maudio::MaudioError) -> Self {
        Self::Maudio(err)
    }
}

impl<T> From<crossbeam_channel::SendError<T>> for AuditoriumError {
    fn from(_: crossbeam_channel::SendError<T>) -> Self {
        Self::ChannelSend
    }
}

impl From<crossbeam_channel::RecvError> for AuditoriumError {
    fn from(_: crossbeam_channel::RecvError) -> Self {
        Self::ChannelRecv
    }
}

impl<T> From<std::sync::mpsc::SendError<T>> for AuditoriumError {
    fn from(_: std::sync::mpsc::SendError<T>) -> Self {
        Self::ChannelSend
    }
}

impl From<std::sync::mpsc::RecvError> for AuditoriumError {
    fn from(_: std::sync::mpsc::RecvError) -> Self {
        Self::ChannelRecv
    }
}
