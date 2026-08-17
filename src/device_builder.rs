use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crossbeam_channel::Sender;
use maudio::{
    audio::{performance::PerformanceProfile, sample_rate::SampleRate},
    device::{device_builder::DeviceBuilderOps, device_id::DeviceId},
};

use crate::{
    AuditoriumError, HostResult,
    device::{CaptureDevice, PlaybackDevice},
    host::{CaptureDeviceStore, HostCommand, HostState, PlayDeviceBuildId, PlaybackDeviceStore},
    store_ops::{HostDispatcher, LiveHost},
};

pub struct PlaybackDeviceBuilder {
    pub(crate) builder: PlayDeviceBuildId,
    pub(crate) sender: Sender<HostCommand>,
    pub(crate) is_shutdown: Arc<AtomicBool>,
    pub(crate) channels: u32,
    pub(crate) sample_rate: SampleRate,
    pub(crate) user_flag: Option<Arc<AtomicBool>>,
}

impl LiveHost for PlaybackDeviceBuilder {
    fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for PlaybackDeviceBuilder {
    fn post<F>(&self, f: F) -> crate::HostResult<()>
    where
        F: FnOnce(&mut crate::host::HostState) -> crate::HostResult<()> + Send + 'static,
    {
        if self.is_shutdown.load(Ordering::Relaxed) {
            return Err(AuditoriumError::HostShutdown);
        }
        self.sender.send(HostCommand::Job(Box::new(f)))?;
        Ok(())
    }
}

impl PlaybackDeviceBuilder {
    pub fn device_id(self, device_id: &DeviceId) -> HostResult<Self> {
        let id = self.builder;
        let dev_id = device_id.clone();
        self.call(move |state| {
            let builder = state
                .play_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.playback_device_id(&dev_id);
            Ok(())
        })?;
        Ok(self)
    }

    pub fn producing_flag(mut self, flag: Arc<AtomicBool>) -> HostResult<Self> {
        self.user_flag = Some(flag);
        Ok(self)
    }

    pub fn channels(mut self, channels: u32) -> HostResult<Self> {
        let id = self.builder;
        self.call(move |state| {
            let builder = state
                .play_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.playback_channels(channels);
            Ok(())
        })?;
        self.channels = channels;
        Ok(self)
    }

    pub fn clipping(self, yes: bool) -> HostResult<Self> {
        let id = self.builder;
        self.call(move |state| {
            let builder = state
                .play_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.clipping(yes);
            Ok(())
        })?;
        Ok(self)
    }

    pub fn performance_mode(self, mode: PerformanceProfile) -> HostResult<Self> {
        let id = self.builder;
        self.call(move |state| {
            let builder = state
                .play_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.performance_profile(mode);
            Ok(())
        })?;
        Ok(self)
    }

    pub fn sample_rate(mut self, sample_rate: SampleRate) -> HostResult<Self> {
        let id = self.builder;
        self.call(move |state| {
            let builder = state
                .play_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.sample_rate(sample_rate);
            Ok(())
        })?;
        self.sample_rate = sample_rate;
        Ok(self)
    }

    // pub fn period_size_frames(self, frames: u32) -> HostResult<Self> {
    //     let id = self.builder;
    //     self.call(move |state| {
    //         let builder = state
    //             .play_device_builders
    //             .values
    //             .get_mut(&id)
    //             .ok_or(AuditoriumError::InvalidDevice)?;
    //         builder.period_size_frames(frames);
    //         Ok(())
    //     })?;
    //     Ok(self)
    // }

    // pub fn period_size_millis(self, millis: u32) -> HostResult<Self> {
    //     let id = self.builder;
    //     self.call(move |state| {
    //         let builder = state
    //             .play_device_builders
    //             .values
    //             .get_mut(&id)
    //             .ok_or(AuditoriumError::InvalidDevice)?;
    //         builder.period_size_millis(millis);
    //         Ok(())
    //     })?;
    //     Ok(self)
    // }

    // pub fn fixed_callback_size(self, yes: bool) -> HostResult<Self> {
    //     let id = self.builder;
    //     self.call(move |state| {
    //         let builder = state
    //             .play_device_builders
    //             .values
    //             .get_mut(&id)
    //             .ok_or(AuditoriumError::InvalidDevice)?;
    //         builder.fixed_callback_size(yes);
    //         Ok(())
    //     })?;
    //     Ok(self)
    // }

    pub(crate) fn insert_builder(
        state: &mut HostState,
        device_store: PlaybackDeviceStore,
        sender: Sender<HostCommand>,
        flag: Arc<AtomicBool>,
    ) -> HostResult<PlaybackDevice> {
        let tracker = device_store.activity.clone();
        let device_id = state.playback_devices.insert(device_store);
        let handle = PlaybackDevice::new(device_id, sender, flag, tracker);
        Ok(handle)
    }

    pub fn build(self) -> HostResult<PlaybackDevice> {
        let id = self.builder;
        let sender = self.sender.clone();
        let shutdown_flag = self.is_shutdown.clone();
        let user_flag = self.user_flag.clone();
        let handle = self.call(move |state| {
            let builder = state
                .play_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            let device_store =
                PlaybackDeviceStore::new(self.channels, self.sample_rate, builder, user_flag)?;
            Self::insert_builder(state, device_store, sender, shutdown_flag)
        })?;
        Ok(handle)
    }
}

pub struct CaptureDeviceBuilder {
    pub(crate) builder: PlayDeviceBuildId,
    pub(crate) sender: Sender<HostCommand>,
    pub(crate) is_shutdown: Arc<AtomicBool>,
    pub(crate) channels: u32,
    pub(crate) sample_rate: SampleRate,
}

impl LiveHost for CaptureDeviceBuilder {
    fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for CaptureDeviceBuilder {
    fn post<F>(&self, f: F) -> crate::HostResult<()>
    where
        F: FnOnce(&mut crate::host::HostState) -> crate::HostResult<()> + Send + 'static,
    {
        if self.is_shutdown.load(Ordering::Relaxed) {
            return Err(AuditoriumError::HostShutdown);
        }
        self.sender.send(HostCommand::Job(Box::new(f)))?;
        Ok(())
    }
}

impl CaptureDeviceBuilder {
    pub fn device_id(self, device_id: &DeviceId) -> HostResult<Self> {
        let id = self.builder;
        let dev_id = device_id.clone();
        self.call(move |state| {
            let builder = state
                .capt_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.capture_device_id(&dev_id);
            Ok(())
        })?;
        Ok(self)
    }

    pub fn channels(mut self, channels: u32) -> HostResult<Self> {
        let id = self.builder;
        self.call(move |state| {
            let builder = state
                .capt_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.capture_channels(channels);
            Ok(())
        })?;
        self.channels = channels;
        Ok(self)
    }

    pub fn clipping(self, yes: bool) -> HostResult<Self> {
        let id = self.builder;
        self.call(move |state| {
            let builder = state
                .capt_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.clipping(yes);
            Ok(())
        })?;
        Ok(self)
    }

    pub fn performance_mode(self, mode: PerformanceProfile) -> HostResult<Self> {
        let id = self.builder;
        self.call(move |state| {
            let builder = state
                .capt_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.performance_profile(mode);
            Ok(())
        })?;
        Ok(self)
    }

    pub fn sample_rate(mut self, sample_rate: SampleRate) -> HostResult<Self> {
        let id = self.builder;
        self.call(move |state| {
            let builder = state
                .capt_device_builders
                .values
                .get_mut(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            builder.sample_rate(sample_rate);
            Ok(())
        })?;
        self.sample_rate = sample_rate;
        Ok(self)
    }

    // pub fn period_size_frames(self, frames: u32) -> HostResult<Self> {
    //     let id = self.builder;
    //     self.call(move |state| {
    //         let builder = state
    //             .capt_device_builders
    //             .values
    //             .get_mut(&id)
    //             .ok_or(AuditoriumError::InvalidDevice)?;
    //         builder.period_size_frames(frames);
    //         Ok(())
    //     })?;
    //     Ok(self)
    // }

    // pub fn period_size_millis(self, millis: u32) -> HostResult<Self> {
    //     let id = self.builder;
    //     self.call(move |state| {
    //         let builder = state
    //             .capt_device_builders
    //             .values
    //             .get_mut(&id)
    //             .ok_or(AuditoriumError::InvalidDevice)?;
    //         builder.period_size_millis(millis);
    //         Ok(())
    //     })?;
    //     Ok(self)
    // }

    // pub fn fixed_callback_size(self, yes: bool) -> HostResult<Self> {
    //     let id = self.builder;
    //     self.call(move |state| {
    //         let builder = state
    //             .capt_device_builders
    //             .values
    //             .get_mut(&id)
    //             .ok_or(AuditoriumError::InvalidDevice)?;
    //         builder.fixed_callback_size(yes);
    //         Ok(())
    //     })?;
    //     Ok(self)
    // }

    pub(crate) fn insert_builder(
        state: &mut HostState,
        device_store: CaptureDeviceStore,
        sender: Sender<HostCommand>,
        flag: Arc<AtomicBool>,
    ) -> HostResult<CaptureDevice> {
        let device_id = state.capture_devices.insert(device_store);
        Ok(CaptureDevice::new(device_id, sender, flag))
    }

    pub fn build(self, path: &Path) -> HostResult<CaptureDevice> {
        let id = self.builder;
        let sender = self.sender.clone();
        let flag = self.is_shutdown.clone();
        let path = path.to_path_buf();
        let handle = self.call(move |state| {
            let mut builder = state
                .capt_device_builders
                .values
                .remove(&id)
                .ok_or(AuditoriumError::InvalidDevice)?;
            let device_store =
                CaptureDeviceStore::new(self.channels, self.sample_rate, &mut builder, &path)?;
            Self::insert_builder(state, device_store, sender, flag)
        })?;
        Ok(handle)
    }
}
