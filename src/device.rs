//! Audio device handles and source creation.
//!
//! This module provides [`PlaybackDevice`] and [`CaptureDevice`], which are
//! handles to audio devices managed by the host.
//!
//! Both device types allow audio sources to be created and attached to their
//! node graphs. Sources can be created from audio files or from generated
//! waveforms, pulse waves, and noise.
//!
//! # Playback
//!
//! [`PlaybackDevice`] represents an output device. Sources created through
//! [`PlaybackDevice`] produce audio that is ultimately sent to the playback
//! device.
//!
//! # Capture
//!
//! [`CaptureDevice`] represents an input device. Captured audio can be routed
//! through the device's node graph, where DSP chains and other processing can
//! be applied before reaching the output.
//!
//! # Device lifecycle
//!
//! A device must be started with [`PlaybackDevice::start_device`] or
//! [`CaptureDevice::start_device`] before it can process audio. Stopping the
//! device with [`PlaybackDevice::stop_device`] or
//! [`CaptureDevice::stop_device`] stops the underlying audio device.
//!
//! Stopping a device is different from pausing playback or recording. Use
//! [`PlaybackDevice::pause_playback`] and [`PlaybackDevice::resume_playback`]
//! for playback, or [`CaptureDevice::pause_recording`] and
//! [`CaptureDevice::resume_recording`] for capture.
//!
//! # Threading
//!
//! Device operations are dispatched to the host thread. The device handles
//! themselves can be cloned and used from other threads.
use std::{
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crossbeam_channel::Sender;
use maudio::{
    audio::wave_shape::WaveFormType,
    data_source::{
        DataSource,
        sources::{
            decoder::{Fs, custom_decoder::CustomDecoder},
            noise::{Noise as RawNoise, NoiseType},
            pulsewave::PulseWave,
            waveform::WaveForm,
        },
    },
    engine::{
        node_graph::{
            NodeGraphOps,
            nodes::{
                NodeOps, NodeState,
                routing::splitter::SplitterNodeBuilder,
                source::source_node::{AttachedSourceNode, AttachedSourceNodeBuilder},
            },
        },
        resource::{
            RmOps, Unknown, rm_source_flags::RmSourceFlags, rm_stream::ResourceManagerStream,
        },
    },
};

use crate::{
    AuditoriumError, HostResult,
    chain::{Dsp, DspChain, DspTarget},
    host::{
        CaptureDeviceId, HostCommand,
        HostedNodes::{self, SplitterNode},
        NodeId, PlaybackDeviceId,
    },
    sources::{audio::Audio, noise::Noise, pulse::Pulse, wave::Wave},
    store_ops::{HostDispatcher, LiveHost, StoreOrigin},
    tracked_source::{PlaybackActivity, TrackedSource},
};

#[derive(Clone)]
pub struct PlaybackDevice(pub(crate) Arc<PlaybackDeviceInner>);

pub(crate) struct PlaybackDeviceInner {
    pub(crate) device: PlaybackDeviceId,
    sender: Sender<HostCommand>,
    is_shutdown: Arc<AtomicBool>,
    activity_track: Rc<PlaybackActivity>,
}

unsafe impl Send for PlaybackDeviceInner {}
unsafe impl Sync for PlaybackDeviceInner {}

impl LiveHost for PlaybackDevice {
    fn is_shutdown(&self) -> bool {
        self.0.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for PlaybackDevice {
    fn post<F>(&self, f: F) -> crate::HostResult<()>
    where
        F: FnOnce(&mut crate::host::HostState) -> crate::HostResult<()> + Send + 'static,
    {
        if self.0.is_shutdown.load(Ordering::Relaxed) {
            return Err(AuditoriumError::HostShutdown);
        }
        self.0.sender.send(HostCommand::Job(Box::new(f)))?;
        Ok(())
    }
}

impl PlaybackDevice {
    pub(crate) fn new(
        device: PlaybackDeviceId,
        sender: Sender<HostCommand>,
        is_shutdown: Arc<AtomicBool>,
        tracker: Rc<PlaybackActivity>,
    ) -> Self {
        Self(Arc::new(PlaybackDeviceInner {
            device,
            sender,
            is_shutdown,
            activity_track: tracker,
        }))
    }

    /// Creates an audio source backed by a file.
    ///
    /// The file is decoded using the configured custom decoder and attached to
    /// this playback device's node graph. The returned [`Audio`] source is
    /// initially stopped.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or decoded, or if the source
    /// cannot be attached to the device's node graph.
    pub fn new_audio<P: AsRef<Path>>(&self, path: P) -> HostResult<Audio> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let path = path.as_ref().to_path_buf();
        let node_id = self.call_playback_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let guard = state.res_man.register_file(&path, RmSourceFlags::NONE)?;
            let stream: ResourceManagerStream<f32, Unknown> = guard
                .build_stream(RmSourceFlags::NONE, None)?
                .into_ready()
                .unwrap();
            let src = TrackedSource::<ResourceManagerStream<f32, Unknown>>::new_decoder(
                state.channels,
                state.sample_rate,
                state.activity.clone(),
                stream,
            )?;
            let mut node = AttachedSourceNodeBuilder::new(node_graph, src).build()?;
            node.attach_output_bus(0, &mut endpoint, 0)?;
            node.set_state(NodeState::Stopped)?;
            let id = state.nodes.insert(HostedNodes::AttachedDecoder(node));
            Ok(id)
        })?;
        Ok(Audio::new(
            node_id,
            StoreOrigin::Playback(id),
            sender,
            self.0.is_shutdown.clone(),
        ))
    }

    /// Creates a waveform generator.
    ///
    /// The generated waveform is attached to this playback device's node graph.
    /// The returned [`Wave`] source is initially stopped.
    ///
    /// `amplitude` controls the output amplitude and `frequency` controls the
    /// waveform frequency in Hz.
    pub fn new_wave(
        &self,
        wave_type: WaveFormType,
        amplitude: f64,
        frequency: f64,
    ) -> HostResult<Wave> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let node_id = self.call_playback_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let src = TrackedSource::<WaveForm<f32>>::new_wave(
                state.channels,
                state.sample_rate,
                wave_type,
                amplitude,
                frequency,
                state.activity.clone(),
            )?;
            let mut node: AttachedSourceNode<DataSource<f32, TrackedSource<WaveForm<f32>>>> =
                AttachedSourceNodeBuilder::new(node_graph, src).build()?;
            node.attach_output_bus(0, &mut endpoint, 0)?;
            node.set_state(NodeState::Stopped)?;
            let id = state.nodes.insert(HostedNodes::AttachedWave(node));
            Ok(id)
        })?;
        Ok(Wave::new(
            node_id,
            StoreOrigin::Playback(id),
            sender,
            self.0.is_shutdown.clone(),
        ))
    }

    /// Creates a pulse-wave generator.
    ///
    /// The generated pulse wave is attached to this playback device's node
    /// graph. The returned [`Pulse`] source is initially stopped.
    ///
    /// `amplitude` controls the output amplitude, `frequency` controls the
    /// frequency in Hz, and `duty_cycle` controls the proportion of each period
    /// for which the signal is high.
    pub fn new_pulse(&self, amplitude: f64, frequency: f64, duty_cycle: f64) -> HostResult<Pulse> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let node_id = self.call_playback_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let src = TrackedSource::<PulseWave<f32>>::new_pulse(
                state.channels,
                state.sample_rate,
                amplitude,
                frequency,
                duty_cycle,
                state.activity.clone(),
            )?;
            let mut node: AttachedSourceNode<DataSource<f32, TrackedSource<PulseWave<f32>>>> =
                AttachedSourceNodeBuilder::new(node_graph, src).build()?;
            node.attach_output_bus(0, &mut endpoint, 0)?;
            node.set_state(NodeState::Stopped)?;
            let id = state.nodes.insert(HostedNodes::AttachedPulse(node));
            Ok(id)
        })?;
        Ok(Pulse::new(
            node_id,
            StoreOrigin::Playback(id),
            sender,
            self.0.is_shutdown.clone(),
        ))
    }

    /// Creates a noise generator.
    ///
    /// The generated noise is attached to this playback device's node graph.
    /// The returned [`Noise`] source is initially stopped.
    ///
    /// `amplitude` controls the output amplitude.
    pub fn new_noise(&self, noise_type: NoiseType, amplitude: f64) -> HostResult<Noise> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let node_id = self.call_playback_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let src = TrackedSource::<RawNoise<f32>>::new_noise(
                state.channels,
                state.sample_rate,
                amplitude,
                noise_type,
                state.activity.clone(),
            )?;
            let mut node: AttachedSourceNode<DataSource<f32, TrackedSource<RawNoise<f32>>>> =
                AttachedSourceNodeBuilder::new(node_graph, src).build()?;
            node.attach_output_bus(0, &mut endpoint, 0)?;
            node.set_state(NodeState::Stopped)?;
            let id = state.nodes.insert(HostedNodes::AttachedNoise(node));
            Ok(id)
        })?;
        Ok(Noise::new(
            node_id,
            StoreOrigin::Playback(id),
            sender,
            self.0.is_shutdown.clone(),
        ))
    }

    /// Pauses playback without stopping the underlying audio device or device.
    ///
    /// While paused, the device remains running but playback sources do not
    /// advance through their audio.
    pub fn pause_playback(&self) -> HostResult<()> {
        let id = self.0.device;
        self.call_playback_device(id, move |store| {
            store.paused.store(true, Ordering::Relaxed);
            Ok(())
        })
    }

    /// Resumes playback after [`PlaybackDevice::pause_playback`].
    pub fn resume_playback(&self) -> HostResult<()> {
        let id = self.0.device;
        self.call_playback_device(id, move |store| {
            store.paused.store(false, Ordering::Relaxed);
            Ok(())
        })
    }

    /// Start the device.
    ///
    /// This allows the device to pull frames from any sources.
    pub fn start_device(&self) -> HostResult<()> {
        let id = self.0.device;
        self.call_playback_device(id, move |store| {
            store
                .device
                .device_start()
                .map_err(|_| AuditoriumError::Other {
                    msg: "Failed to start device".into(),
                })
        })
    }

    /// Stops the device.
    ///
    /// It is not recommended to use this for pausing playback. Use [`PlaybackDevice::pause_playback`]
    pub fn stop_device(&self) -> HostResult<()> {
        let id = self.0.device;
        self.call_playback_device(id, move |store| {
            store
                .device
                .device_stop()
                .map_err(|_| AuditoriumError::Other {
                    msg: "Failed to start device".into(),
                })
        })
    }

    /// Returns whether any playback source is currently producing audio.
    ///
    /// This does not indicate whether the underlying device is running. It tracks
    /// whether audio sources are actively producing frames for the engine, so a
    /// running device with no active sources returns `false`.
    ///
    /// Silence produced by an active source is still considered producing audio.
    pub fn is_producing(&self) -> bool {
        self.0.activity_track.tracker.load(Ordering::Relaxed) != 0
    }
}

#[derive(Clone)]
pub struct CaptureDevice(pub(crate) Arc<CaptureDeviceInner>);

pub(crate) struct CaptureDeviceInner {
    pub(crate) device: CaptureDeviceId,
    pub(crate) sender: Sender<HostCommand>,
    pub(crate) is_shutdown: Arc<AtomicBool>,
}

unsafe impl Send for CaptureDeviceInner {}
unsafe impl Sync for CaptureDeviceInner {}

impl LiveHost for CaptureDevice {
    fn is_shutdown(&self) -> bool {
        self.0.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for CaptureDevice {
    fn post<F>(&self, f: F) -> crate::HostResult<()>
    where
        F: FnOnce(&mut crate::host::HostState) -> crate::HostResult<()> + Send + 'static,
    {
        if self.0.is_shutdown.load(Ordering::Relaxed) {
            return Err(AuditoriumError::HostShutdown);
        }
        self.0.sender.send(HostCommand::Job(Box::new(f)))?;
        Ok(())
    }
}

impl Dsp for CaptureDevice {
    fn dst_target<'a>(&'a self) -> DspTarget<'a> {
        DspTarget::Capture(self)
    }

    fn splitter(&self, out_bus_count: u32) -> HostResult<NodeId> {
        let id = self.0.device;
        self.call_capture_device(id, move |state| {
            let node_graph = &state.node_graph;
            let spliter = SplitterNodeBuilder::new(node_graph, out_bus_count).build()?;
            let node = SplitterNode(spliter);
            Ok(state.nodes.insert(node))
        })
    }
}

impl CaptureDevice {
    pub(crate) fn new(
        device: CaptureDeviceId,
        sender: Sender<HostCommand>,
        is_shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self(Arc::new(CaptureDeviceInner {
            device,
            sender,
            is_shutdown,
        }))
    }

    /// Creates an audio source backed by a file.
    ///
    /// The file is decoded using the configured custom decoder and attached to
    /// this playback device's node graph. The returned [`Audio`] source is
    /// initially stopped.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or decoded, or if the source
    /// cannot be attached to the device's node graph.
    pub fn new_audio<P: AsRef<Path>>(&self, path: P) -> HostResult<Audio> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let path = path.as_ref().to_path_buf();
        let node_id = self.call_capture_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let guard = state.res_man.register_file(&path, RmSourceFlags::NONE)?;
            let stream: ResourceManagerStream<f32, Unknown> = guard
                .build_stream(RmSourceFlags::NONE, None)?
                .into_ready()
                .unwrap();
            let src = TrackedSource::<CustomDecoder<f32, Fs>>::new_decoder(
                state.channels,
                state.sample_rate,
                PlaybackActivity::new(None),
                stream,
            )?;
            let mut node: AttachedSourceNode<
                DataSource<f32, TrackedSource<ResourceManagerStream<f32, Unknown>>>,
            > = AttachedSourceNodeBuilder::new(node_graph, src).build()?;
            node.attach_output_bus(0, &mut endpoint, 0)?;
            node.set_state(NodeState::Stopped)?;
            let id = state.nodes.insert(HostedNodes::AttachedDecoder(node));
            Ok(id)
        })?;
        Ok(Audio::new(
            node_id,
            StoreOrigin::Capture(id),
            sender,
            self.0.is_shutdown.clone(),
        ))
    }

    /// Creates a waveform generator.
    ///
    /// The generated waveform is attached to this playback device's node graph.
    /// The returned [`Wave`] source is initially stopped.
    ///
    /// `amplitude` controls the output amplitude and `frequency` controls the
    /// waveform frequency in Hz.
    pub fn new_wave(
        &self,
        wave_type: WaveFormType,
        amplitude: f64,
        frequency: f64,
    ) -> HostResult<Wave> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let node_id = self.call_capture_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let src = TrackedSource::<WaveForm<f32>>::new_wave(
                state.channels,
                state.sample_rate,
                wave_type,
                amplitude,
                frequency,
                PlaybackActivity::new(None),
            )?;
            let mut node: AttachedSourceNode<DataSource<f32, TrackedSource<WaveForm<f32>>>> =
                AttachedSourceNodeBuilder::new(node_graph, src).build()?;
            node.attach_output_bus(0, &mut endpoint, 0)?;
            node.set_state(NodeState::Stopped)?;
            let id = state.nodes.insert(HostedNodes::AttachedWave(node));
            Ok(id)
        })?;
        Ok(Wave::new(
            node_id,
            StoreOrigin::Capture(id),
            sender,
            self.0.is_shutdown.clone(),
        ))
    }

    /// Creates a pulse-wave generator.
    ///
    /// The generated pulse wave is attached to this playback device's node
    /// graph. The returned [`Pulse`] source is initially stopped.
    ///
    /// `amplitude` controls the output amplitude, `frequency` controls the
    /// frequency in Hz, and `duty_cycle` controls the proportion of each period
    /// for which the signal is high.
    pub fn new_pulse(&self, amplitude: f64, frequency: f64, duty_cycle: f64) -> HostResult<Pulse> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let node_id = self.call_capture_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let src = TrackedSource::<PulseWave<f32>>::new_pulse(
                state.channels,
                state.sample_rate,
                amplitude,
                frequency,
                duty_cycle,
                PlaybackActivity::new(None),
            )?;
            let mut node: AttachedSourceNode<DataSource<f32, TrackedSource<PulseWave<f32>>>> =
                AttachedSourceNodeBuilder::new(node_graph, src).build()?;
            node.attach_output_bus(0, &mut endpoint, 0)?;
            node.set_state(NodeState::Stopped)?;
            let id = state.nodes.insert(HostedNodes::AttachedPulse(node));
            Ok(id)
        })?;
        Ok(Pulse::new(
            node_id,
            StoreOrigin::Capture(id),
            sender,
            self.0.is_shutdown.clone(),
        ))
    }

    /// Creates a noise generator.
    ///
    /// The generated noise is attached to this playback device's node graph.
    /// The returned [`Noise`] source is initially stopped.
    ///
    /// `amplitude` controls the output amplitude.
    pub fn new_noise(&self, noise_type: NoiseType, amplitude: f64) -> HostResult<Noise> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let node_id = self.call_capture_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let src = TrackedSource::<RawNoise<f32>>::new_noise(
                state.channels,
                state.sample_rate,
                amplitude,
                noise_type,
                PlaybackActivity::new(None),
            )?;
            let mut node: AttachedSourceNode<DataSource<f32, TrackedSource<RawNoise<f32>>>> =
                AttachedSourceNodeBuilder::new(node_graph, src).build()?;
            node.attach_output_bus(0, &mut endpoint, 0)?;
            node.set_state(NodeState::Stopped)?;
            let id = state.nodes.insert(HostedNodes::AttachedNoise(node));
            Ok(id)
        })?;
        Ok(Noise::new(
            node_id,
            StoreOrigin::Capture(id),
            sender,
            self.0.is_shutdown.clone(),
        ))
    }

    /// Creates a DSP chain starting at this capture device.
    ///
    /// The returned chain can be used to add DSP nodes to the capture signal
    /// path.
    pub fn dsp<'a>(&'a self) -> DspChain<'a> {
        DspChain::apply_chain(DspChain::new(), DspTarget::Capture(self))
    }

    /// Inserts a splitter and outputs `N` number of dsp chains
    ///
    /// All the dsp chains will be mixed back together at the output
    pub fn dsp_split<'a, const N: usize>(&'a self) -> [DspChain<'a>; N] {
        [DspChain::apply_chain(DspChain::new(), DspTarget::Capture(self)); N]
    }

    pub fn pause_recording(&self) -> HostResult<()> {
        let id = self.0.device;
        self.call_capture_device(id, move |store| {
            store.paused.store(true, Ordering::Relaxed);
            Ok(())
        })
    }

    pub fn resume_recording(&self) -> HostResult<()> {
        let id = self.0.device;
        self.call_capture_device(id, move |store| {
            store.paused.store(false, Ordering::Relaxed);
            Ok(())
        })
    }

    /// Start the device.
    ///
    /// This allows the device to output captured sound and any other sources to output as well.
    pub fn start_device(&self) -> HostResult<()> {
        let id = self.0.device;
        self.call_capture_device(id, move |store| {
            store
                .device
                .device_start()
                .map_err(|_| AuditoriumError::Other {
                    msg: "Failed to start device".into(),
                })
        })
    }

    /// Stops the device.
    ///
    /// It is not recommended to use this for pausing recording. Use [`CaptureDevice::pause_recording`]
    pub fn stop_device(&self) -> HostResult<()> {
        let id = self.0.device;
        self.call_capture_device(id, move |store| {
            store
                .device
                .device_stop()
                .map_err(|_| AuditoriumError::Other {
                    msg: "Failed to start device".into(),
                })
        })
    }
}
