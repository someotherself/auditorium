use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
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
    engine::node_graph::{
        NodeGraphOps,
        nodes::{
            NodeOps, NodeState,
            routing::splitter::SplitterNodeBuilder,
            source::source_node::{AttachedSourceNode, AttachedSourceNodeBuilder},
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
    activity_track: Arc<PlaybackActivity>,
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
        tracker: Arc<PlaybackActivity>,
    ) -> Self {
        Self(Arc::new(PlaybackDeviceInner {
            device,
            sender,
            is_shutdown,
            activity_track: tracker,
        }))
    }

    /// Create a new file based audio source
    pub fn new_audio<P: AsRef<Path>>(&self, path: P) -> HostResult<Audio> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let path = path.as_ref().to_path_buf();
        let node_id = self.call_playback_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let src = TrackedSource::<CustomDecoder<f32, Fs>>::new_decoder(
                &path,
                state.channels,
                state.sample_rate,
                state.activity.clone(),
            )?;
            let mut node: AttachedSourceNode<
                DataSource<f32, TrackedSource<CustomDecoder<f32, Fs>>>,
            > = AttachedSourceNodeBuilder::new(node_graph, src).build()?;
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

    /// Create a new waveform generator
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

    /// Create a new pulsewave generator
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

    /// Create a new noise generator
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

    /// Returns true if there is any audio sources audio to the engine
    ///
    /// This tracks the absence of audio, not silenced audio.
    pub fn is_producing(&self) -> bool {
        self.0.activity_track.0.load(Ordering::Relaxed) != 0
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

    /// Create a new file based audio source
    pub fn new_audio<P: AsRef<Path>>(&self, path: P) -> HostResult<Audio> {
        let id = self.0.device;
        let sender = self.0.sender.clone();
        let path = path.as_ref().to_path_buf();
        let node_id = self.call_capture_device(id, move |state| {
            let node_graph = &state.node_graph;
            let mut endpoint = node_graph.endpoint();
            let src = TrackedSource::<CustomDecoder<f32, Fs>>::new_decoder(
                &path,
                state.channels,
                state.sample_rate,
                Arc::new(PlaybackActivity(AtomicU32::new(0))),
            )?;
            let mut node: AttachedSourceNode<
                DataSource<f32, TrackedSource<CustomDecoder<f32, Fs>>>,
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

    /// Create a new waveform generator
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
                Arc::new(PlaybackActivity(AtomicU32::new(0))),
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

    /// Create a new pulsewave generator
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
                Arc::new(PlaybackActivity(AtomicU32::new(0))),
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

    /// Create a new noise generator
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
                Arc::new(PlaybackActivity(AtomicU32::new(0))),
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

    /// Start a dsp chain for this capture device
    pub fn dsp<'a>(&'a self) -> DspChain<'a> {
        DspChain::apply_chain(DspChain::new(), DspTarget::Capture(self))
    }

    /// Inserts a splitter and outputs `N` number of dsp chains
    ///
    /// All the dsp chains will be mixed back together at the output
    pub fn dsp_split<'a, const N: usize>(&'a self) -> [DspChain<'a>; N] {
        [DspChain::apply_chain(DspChain::new(), DspTarget::Capture(self)); N]
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
