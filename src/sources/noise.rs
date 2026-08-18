//! Noise generator
//!
//! This module provide [`Noise`], a handle for controlling a Noise
//! generator managed by a [`Host`](crate::host).
//!
//! `Noise` does not directly own the underlying pulse generator. Instead, the
//! source is stored in the node graph associated with the device that created
//! it. Operations performed through `Noise` are dispatched to the host thread,
//! where the source and its node can be safely accessed.
//!
//! `Noise` can be cloned. Each clone refers to the same underlying noise generator.
//! The source remains available while at least one `Noise` handle exists.
//!
//! Dropping the final handle removes the source from its device's node graph.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crossbeam_channel::Sender;
use maudio::{
    MaResult,
    data_source::{pcm_source::PcmSource, sources::noise::Noise as RawNoise},
    engine::node_graph::nodes::{NodeOps, NodeState, routing::splitter::SplitterNodeBuilder},
};

use crate::{
    AuditoriumError, HostResult,
    chain::{Dsp, DspTarget},
    host::{
        HostCommand,
        HostedNodes::{self, SplitterNode},
        NodeId, SourceId,
    },
    store_ops::{HostDispatcher, LiveHost, StoreOrigin},
    tracked_source::TrackedSource,
};

/// A handle for a Waveform generator.
///
/// `Noise` provides control over an noise generator stored in a device's node
/// graph. It can be used to start and stop playback, seek, configure looping
/// and volume, query the current position and length, and attach DSP effects.
///
/// `Noise` does not directly own the underlying source. Instead, operations are
/// dispatched to the host thread, which owns the device and its node graph.
///
/// `Noise` can be cloned, and all clones refer to the same underlying source.
/// The source is removed from the node graph when the final handle is dropped.
///
/// Most operations return [`AuditoriumError::HostShutdown`] if the associated
/// host has already been shut down.
#[derive(Clone)]
pub struct Noise(pub(crate) Arc<NoiseInner>);

pub(crate) struct NoiseInner {
    pub(crate) sender: Sender<HostCommand>,
    pub(crate) store_id: StoreOrigin,
    pub(crate) id: SourceId,
    pub(crate) is_shutdown: Arc<AtomicBool>,
}

impl LiveHost for NoiseInner {
    fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for NoiseInner {
    fn post<F>(&self, f: F) -> HostResult<()>
    where
        F: FnOnce(&mut crate::host::HostState) -> HostResult<()> + Send + 'static,
    {
        if self.is_shutdown.load(Ordering::Relaxed) {
            return Err(AuditoriumError::HostShutdown);
        }
        self.sender.send(HostCommand::Job(Box::new(f)))?;
        Ok(())
    }
}

impl Dsp for Noise {
    fn dst_target<'a>(&'a self) -> DspTarget<'a> {
        DspTarget::Noise(self)
    }

    fn splitter(&self, out_bus_count: u32) -> HostResult<NodeId> {
        let id = self.0.store_id;
        self.0.call_store_impl(id, move |state| {
            let node_graph = state.node_graph();
            let spliter = SplitterNodeBuilder::new(node_graph, out_bus_count).build()?;
            let node = SplitterNode(spliter);
            Ok(state.mut_nodes().insert(node))
        })
    }
}

impl Noise {
    pub(crate) fn new(
        id: SourceId,
        store_id: StoreOrigin,
        sender: Sender<HostCommand>,
        flag: Arc<AtomicBool>,
    ) -> Self {
        Noise(Arc::new(NoiseInner {
            sender,
            store_id,
            id,
            is_shutdown: flag,
        }))
    }

    pub fn set_volume(&self, volume: f32) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedNoise(value) => value
                    .as_node()
                    .set_output_bus_volume(0, volume)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    pub fn get_volume(&self) -> HostResult<f32> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedNoise(value) => Ok(value.as_node().output_bus_volume(0)),
                _ => unreachable!(),
            }
        })
    }

    pub fn start_audio(&self) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedNoise(value) => {
                    value.source().set_active(true);
                    value
                        .as_node()
                        .set_state(NodeState::Started)
                        .map_err(AuditoriumError::from)
                }
                _ => unreachable!(),
            }
        })
    }

    pub fn stop_audio(&self) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedNoise(value) => value
                    .as_node()
                    .set_state(NodeState::Stopped)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    pub fn set_amplitude(&self, amplitude: f64) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedNoise(value) => value
                    .source_mut()
                    .inner_source_mut()
                    .set_amplitude(amplitude)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }
}

impl PcmSource<f32> for TrackedSource<RawNoise<f32>> {
    fn fill_pcm_frames(
        &mut self,
        out: &mut [<f32 as maudio::pcm_frames::PcmFormat>::PcmUnit],
        _ctx: &mut maudio::data_source::SourceContext,
    ) -> MaResult<usize> {
        let frames = self.source.read_pcm_frames_into(out).unwrap_or(0);

        self.set_active(frames != 0);

        Ok(frames)
    }
}

impl Drop for NoiseInner {
    fn drop(&mut self) {
        let id = self.id;
        let store_id = self.store_id;
        let _ = self.call_store_impl(store_id, move |state| {
            let _ = state.mut_nodes().values.remove(&id);
            Ok(())
        });
    }
}
