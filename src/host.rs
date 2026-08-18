//! Experimental feature

use std::{
    collections::HashMap,
    fmt::Debug,
    marker::PhantomData,
    path::Path,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

use maudio::{
    audio::sample_rate::SampleRate,
    data_source::{
        DataSource,
        sources::{
            buffer::AudioBufferBuilder,
            decoder::{Fs, custom_decoder::CustomDecoder},
            noise::Noise,
            pulsewave::PulseWave,
            waveform::WaveForm,
        },
    },
    device::{
        Device as RawDevice,
        device_builder::{
            CaptureDeviceBuilder as RawCaptureDeviceBuilder, DeviceBuilder, DeviceBuilderOps,
            PlaybackDeviceBuilder as RawPlaybackDeviceBuilder,
        },
    },
    encoder::EncoderBuilder,
    engine::node_graph::{
        NodeGraph, NodeGraphOps,
        node_graph_builder::NodeGraphBuilder,
        nodes::{
            NodeOps,
            effects::delay::DelayNode,
            filters::{
                biquad::BiquadNode, hishelf::HiShelfNode, hpf::HpfNode, loshelf::LoShelfNode,
                lpf::LpfNode, notch::NotchNode, peak::PeakNode,
            },
            routing::splitter::{SplitterNode, SplitterNodeBuilder},
            source::source_node::{AttachedSourceNode, AttachedSourceNodeBuilder},
        },
    },
};

use crate::{
    AuditoriumError, HostResult,
    device_builder::{CaptureDeviceBuilder, PlaybackDeviceBuilder},
    store_ops::{HostDispatcher, LiveHost},
    tracked_source::{PlaybackActivity, TrackedSource},
};

pub(crate) type NodeId = u64;
pub(crate) type SourceId = u64;
pub(crate) type DspId = u64;
pub(crate) type PlaybackDeviceId = u64;
pub(crate) type CaptureDeviceId = u64;
pub(crate) type PlayDeviceBuildId = u64;
pub(crate) type CaptDeviceBuildId = u64;

pub(crate) struct HostState<'a> {
    pub(crate) playback_devices: Store<PlaybackDeviceId, PlaybackDeviceStore>,
    pub(crate) capture_devices: Store<CaptureDeviceId, CaptureDeviceStore>,
    pub(crate) play_device_builders: Store<PlayDeviceBuildId, RawPlaybackDeviceBuilder<'a, f32>>,
    pub(crate) capt_device_builders: Store<CaptDeviceBuildId, RawCaptureDeviceBuilder<'a, f32>>,
}

#[non_exhaustive]
pub(crate) enum HostedNodes {
    AttachedDecoder(AttachedSourceNode<DataSource<f32, TrackedSource<CustomDecoder<f32, Fs>>>>),
    AttachedWave(AttachedSourceNode<DataSource<f32, TrackedSource<WaveForm<f32>>>>),
    AttachedPulse(AttachedSourceNode<DataSource<f32, TrackedSource<PulseWave<f32>>>>),
    AttachedNoise(AttachedSourceNode<DataSource<f32, TrackedSource<Noise<f32>>>>),
    DelayNode(DelayNode),
    BiquadNode(BiquadNode),
    HiShelfNode(HiShelfNode),
    LoShelfNode(LoShelfNode),
    LpfNode(LpfNode),
    HpfNode(HpfNode),
    NotchNode(NotchNode),
    PeakNode(PeakNode),
    SplitterNode(SplitterNode),
}

impl Debug for HostedNodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AttachedDecoder(_) => write!(f, "AudioSource"),
            Self::AttachedWave(_) => write!(f, "WaveForm"),
            Self::AttachedPulse(_) => write!(f, "PulseWave"),
            Self::AttachedNoise(_) => write!(f, "Noise"),
            Self::BiquadNode(_) => write!(f, "BiquadNode"),
            Self::HiShelfNode(_) => write!(f, "HiShelfNode"),
            Self::LoShelfNode(_) => write!(f, "LoShelfNode"),
            Self::HpfNode(_) => write!(f, "HpfNode"),
            Self::LpfNode(_) => write!(f, "LpfNode"),
            Self::NotchNode(_) => write!(f, "NotchNode"),
            Self::PeakNode(_) => write!(f, "PeakNode"),
            Self::DelayNode(_) => write!(f, "DelayNode"),
            Self::SplitterNode(_) => write!(f, "SplitterNode"),
        }
    }
}

impl<'a> HostState<'a> {
    pub(crate) fn new() -> Self {
        Self {
            playback_devices: Store::new(),
            capture_devices: Store::new(),
            play_device_builders: Store::new(),
            capt_device_builders: Store::new(),
        }
    }

    pub(crate) fn get_playback_device_store(
        &mut self,
        device: PlaybackDeviceId,
    ) -> HostResult<&mut PlaybackDeviceStore> {
        self.playback_devices
            .values
            .get_mut(&device)
            .ok_or(AuditoriumError::InvalidDevice)
    }

    pub(crate) fn get_capture_device_store(
        &mut self,
        device: PlaybackDeviceId,
    ) -> HostResult<&mut CaptureDeviceStore> {
        self.capture_devices
            .values
            .get_mut(&device)
            .ok_or(AuditoriumError::InvalidDevice)
    }
}

impl Drop for HostShared {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

pub struct CaptureDeviceStore {
    pub(crate) device: RawDevice<f32>,
    pub(crate) device_node: NodeId,
    pub(crate) node_graph: NodeGraph,
    pub(crate) nodes: Store<NodeId, HostedNodes>,
    pub(crate) channels: u32,
    pub(crate) sample_rate: SampleRate,
    pub(crate) paused: Arc<AtomicBool>,
}

impl CaptureDeviceStore {
    pub(crate) fn new(
        channels: u32,
        sample_rate: SampleRate,
        builder: &mut RawCaptureDeviceBuilder<f32>,
        path: &Path,
    ) -> HostResult<Self> {
        let frames: usize = 1000;
        let node_graph = NodeGraphBuilder::new(channels).build()?;
        let mut reader = node_graph.try_acquire_reader()?;
        let mut endpoint = node_graph.endpoint();

        // This gives us an audio buffer that does not have a source
        // Later, we can bind it to the input buffer of the device callback
        let buffer_base = AudioBufferBuilder::base_ref_f32(channels, frames as u64)?;

        // Add the AudioBuffer to a Source Node and connect it to the endpoint input bus
        let mut src_node = AttachedSourceNodeBuilder::new(&node_graph, buffer_base).build()?;

        // The source node must live in the callback so we connect it to a splitter.
        // This way, it's easier to connect it to a dsp node in the future.
        let mut splitter = SplitterNodeBuilder::new(&node_graph, channels).build()?;
        src_node.attach_output_bus(0, &mut splitter, 0)?;
        splitter.attach_output_bus(0, &mut endpoint, 0)?;

        let mut store = Store::<NodeId, HostedNodes>::new();
        let dev_node_id = store.insert(HostedNodes::SplitterNode(splitter));

        let mut encoder = EncoderBuilder::new_f32(channels, sample_rate)
            .wav()
            .build_path(path)?;

        // We need an intermediary buffer between the endpoint and the encoder
        let mut out_buff = vec![0.0; frames * channels as usize];

        let paused = Arc::new(AtomicBool::new(false));
        let pause_clone = paused.clone();

        let device = builder
            .capture_channels(channels)
            .sample_rate(sample_rate)
            .period_size_frames(frames as u32)
            .fixed_callback_size(true)
            .with_callback(move |_, input: &[f32]| {
                if pause_clone.load(Ordering::Relaxed) {
                    return;
                }

                if !input.len().is_multiple_of(channels as usize) {
                    eprintln!("Misaligned capture input: {} samples", input.len());
                    return;
                }

                let Ok(_bound_buffer) = src_node.source_mut().bind(input) else {
                    eprintln!("Failed to bind capture buffer");
                    return;
                };

                let output = &mut out_buff[..input.len()];

                let frames_read = match reader.read_pcm_frames_into(output) {
                    Ok(frames_read) => frames_read,
                    Err(err) => {
                        eprintln!("Node graph read failed: {err:?}");
                        return;
                    }
                };

                let samples_read = frames_read * channels as usize;

                if let Err(err) = encoder.write_pcm_frames(&output[..samples_read]) {
                    eprintln!("Encoder write failed: {err:?}");
                }
            })?;

        Ok(CaptureDeviceStore {
            device,
            device_node: dev_node_id,
            node_graph,
            nodes: store,
            channels,
            sample_rate,
            paused,
        })
    }
}

pub(crate) struct PlaybackDeviceStore {
    pub(crate) activity: Rc<PlaybackActivity>,
    pub(crate) device: RawDevice<f32>,
    pub(crate) node_graph: NodeGraph,
    pub(crate) nodes: Store<NodeId, HostedNodes>,
    pub(crate) channels: u32,
    pub(crate) sample_rate: SampleRate,
    pub(crate) paused: Arc<AtomicBool>,
}

impl PlaybackDeviceStore {
    pub(crate) fn new(
        channels: u32,
        sample_rate: SampleRate,
        builder: &mut RawPlaybackDeviceBuilder<f32>,
        user_flag: Option<Arc<AtomicBool>>,
    ) -> HostResult<Self> {
        let node_graph = NodeGraphBuilder::new(channels).build()?;
        let mut reader = node_graph.try_acquire_reader()?;

        let paused = Arc::new(AtomicBool::new(false));
        let pause_clone = paused.clone();

        let device =
            builder
                .playback_channels(channels)
                .with_callback(move |_, out: &mut [f32]| {
                    if pause_clone.load(Ordering::Relaxed) {
                        out.fill(0.0);
                        return;
                    }

                    // The node graph always outputs silence is there are no sources
                    // and always satisfies the device's requested frame count
                    let _ = reader.read_pcm_frames_into(out);
                })?;

        Ok(PlaybackDeviceStore {
            activity: PlaybackActivity::new(user_flag),
            device,
            node_graph,
            nodes: Store::<NodeId, HostedNodes>::new(),
            channels,
            sample_rate,
            paused,
        })
    }
}
pub(crate) struct Store<Id, T> {
    pub(crate) next: u64,
    pub(crate) values: HashMap<u64, T>,
    _marker: PhantomData<Id>,
}

impl<ID, T> Store<ID, T> {
    fn new() -> Self {
        Self {
            next: 0,
            values: HashMap::<u64, T>::new(),
            _marker: PhantomData,
        }
    }

    pub(crate) fn insert(&mut self, item: T) -> u64 {
        let id = self.next;
        self.next += 1;
        self.values.insert(id, item);
        id
    }
}

pub(crate) enum HostCommand {
    Job(Job),
    Shutdown,
}

type Job = Box<dyn FnOnce(&mut HostState) -> HostResult<()> + Send + 'static>;

#[derive(Clone)]
pub struct Host(Arc<HostShared>);

pub struct HostShared {
    sender: crossbeam_channel::Sender<HostCommand>,
    is_shutdown: Arc<AtomicBool>,
    thread: Arc<EngineThread>,
}

struct EngineThread {
    handle: Mutex<Option<JoinHandle<HostResult<()>>>>,
}

unsafe impl Send for HostShared {}
unsafe impl Sync for HostShared {}

impl HostShared {
    fn join(&self) -> HostResult<()> {
        if let Some(handle) = self.thread.handle.lock().unwrap().take() {
            handle.join().map_err(|_| AuditoriumError::ThreadJoin)??;
        }
        Ok(())
    }

    fn shutdown(&self) -> HostResult<()> {
        let already_shutdown = self.is_shutdown.swap(true, Ordering::AcqRel);

        if !already_shutdown {
            // Best effort. Don't return yet. Must try joining the handle.
            let _ = self.sender.send(HostCommand::Shutdown);
        }

        let _ = self.join();

        Ok(())
    }
}

impl LiveHost for Host {
    fn is_shutdown(&self) -> bool {
        self.0.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for Host {
    fn post<F>(&self, f: F) -> HostResult<()>
    where
        F: FnOnce(&mut HostState) -> HostResult<()> + Send + 'static,
    {
        if self.0.is_shutdown.load(Ordering::Relaxed) {
            return Err(AuditoriumError::HostShutdown);
        }
        self.0.sender.send(HostCommand::Job(Box::new(f)))?;
        Ok(())
    }
}

impl Host {
    /// Create a new host
    pub fn spawn() -> HostResult<Host> {
        let (tx, rx) = crossbeam_channel::bounded(500);

        let is_shutdown = Arc::new(AtomicBool::new(false));

        let join = std::thread::spawn(move || -> HostResult<()> {
            let mut state = HostState::new();

            while let Ok(command) = rx.recv() {
                match command {
                    HostCommand::Job(job) => {
                        job(&mut state)?;
                    }
                    HostCommand::Shutdown => {
                        break;
                    }
                }
            }

            Ok(())
        });

        Ok(Host(Arc::new(HostShared {
            sender: tx,
            is_shutdown,
            thread: Arc::new(EngineThread {
                handle: Mutex::new(Some(join)),
            }),
        })))
    }

    /// Create a new capture device inside this `Host`
    ///
    /// If `channels` and `sample rate` is not selected,
    /// default vales of `2` and `44_100` respectively will be used.
    pub fn build_capture_device(&self) -> HostResult<CaptureDeviceBuilder> {
        let id = self.call(|state| {
            let mut builder = DeviceBuilder::capture().f32();
            builder.capture_channels(2).sample_rate(SampleRate::Sr44100);
            let id = state.capt_device_builders.insert(builder);
            Ok(id)
        })?;
        let capt_dev = CaptureDeviceBuilder {
            builder: id,
            sender: self.0.sender.clone(),
            is_shutdown: self.0.is_shutdown.clone(),
            channels: 2,                      // default value
            sample_rate: SampleRate::Sr44100, // default value
        };
        let capt_dev = capt_dev.channels(2)?;
        let capt_dev = capt_dev.sample_rate(SampleRate::Sr44100)?;
        Ok(capt_dev)
    }

    /// Create a new playback device inside this `Host`
    ///
    /// If `channels` and `sample rate` is not selected,
    /// default vales of `2` and `44_100` respectively will be used.
    pub fn build_playback_device(&self) -> HostResult<PlaybackDeviceBuilder> {
        let id = self.call(|state| {
            let mut builder: RawPlaybackDeviceBuilder<'_, f32> = DeviceBuilder::playback().f32();
            builder
                .playback_channels(2)
                .sample_rate(SampleRate::Sr44100);
            let id = state.play_device_builders.insert(builder);
            Ok(id)
        })?;
        let play_dev = PlaybackDeviceBuilder {
            builder: id,
            sender: self.0.sender.clone(),
            is_shutdown: self.0.is_shutdown.clone(),
            channels: 2,                      // default value
            sample_rate: SampleRate::Sr44100, // default value
            user_flag: None,
        };
        let play_dev = play_dev.channels(2)?;
        let play_dev = play_dev.sample_rate(SampleRate::Sr44100)?;
        Ok(play_dev)
    }

    /// Shutdown this host
    ///
    /// Calling this is optional. The host will also shutdown when all instances of [`Host`] are dropped.
    ///
    /// Any handle still alive after calling this will return an error.
    pub fn shutdown(&self) -> HostResult<()> {
        self.0.shutdown()
    }
}
