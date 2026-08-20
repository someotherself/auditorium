//! Decoder backed audio source control.
//!
//! This module provides [`Audio`], a handle for controlling a decoder-backed
//! audio source managed by a [`Host`](crate::host).
//!
//! `Audio` does not directly own the underlying audio source. Instead, the
//! source is stored in the node graph associated with the device that created
//! it. Operations performed through `Audio` are dispatched to the host thread,
//! where the source and its node can be safely accessed.
//!
//! `Audio` can be cloned. Each clone refers to the same underlying audio source.
//! The source remains available while at least one `Audio` handle exists.
//!
//! Dropping the final handle removes the source from its device's node graph.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crossbeam_channel::Sender;
use maudio::{
    MaResult,
    data_source::{DataSourceOps, pcm_source::PcmSource},
    engine::{
        node_graph::nodes::{NodeOps, NodeState, routing::splitter::SplitterNodeBuilder},
        resource::{Unknown, rm_stream::ResourceManagerStream},
    },
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

/// A handle to a decoder-backed audio source.
///
/// `Audio` provides control over an audio source stored in a device's node
/// graph. It can be used to start and stop playback, seek, configure looping
/// and volume, query the current position and length, and attach DSP effects.
///
/// `Audio` does not directly own the underlying source. Instead, operations are
/// dispatched to the host thread, which owns the device and its node graph.
///
/// `Audio` can be cloned, and all clones refer to the same underlying source.
/// The source is removed from the node graph when the final handle is dropped.
///
/// Most operations return [`AuditoriumError::HostShutdown`] if the associated
/// host has already been shut down.
#[derive(Clone)]
pub struct Audio(pub(crate) Arc<AudioInner>);

pub(crate) struct AudioInner {
    pub(crate) sender: Sender<HostCommand>,
    pub(crate) store_id: StoreOrigin,
    pub(crate) id: SourceId,
    pub(crate) is_shutdown: Arc<AtomicBool>,
}

impl LiveHost for AudioInner {
    fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for AudioInner {
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

impl Dsp for Audio {
    fn dst_target<'a>(&'a self) -> DspTarget<'a> {
        DspTarget::Audio(self)
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

impl Audio {
    pub(crate) fn new(
        id: SourceId,
        store_id: StoreOrigin,
        sender: Sender<HostCommand>,
        flag: Arc<AtomicBool>,
    ) -> Self {
        Audio(Arc::new(AudioInner {
            sender,
            store_id,
            id,
            is_shutdown: flag,
        }))
    }

    /// Seeks the audio source to the given position in seconds.
    pub fn seek_to_second(&self, second: f32) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        // let frame = second * self.sa
        self.0.call_store_impl(device_id, move |store| {
            let frame = second * u32::from(store.sample_rate()) as f32;
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .source_mut()
                    .source
                    .seek_to_pcm_frame(frame as u64)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    /// Seeks the audio source to the given PCM frame.
    pub fn seek_to_pcm_frame(&self, frame: u64) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .source_mut()
                    .seek_to_pcm_frame(frame)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    /// Enables or disables looping for this audio source.
    ///
    /// When looping is enabled, playback continues from the beginning after
    /// reaching the end of the source.
    pub fn set_looping(&self, yes: bool) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .source_mut()
                    .set_looping(yes)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    /// Returns the current position of the cursor in pcm frames
    pub fn cursor_seconds(&self) -> HostResult<f32> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => {
                    value.source().set_active(true);
                    value
                        .source()
                        .cursor_in_seconds()
                        .map_err(AuditoriumError::from)
                }
                _ => unreachable!(),
            }
        })
    }

    /// Returns the current position of the cursor in pcm frames
    pub fn cursor_pcm(&self) -> HostResult<u64> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => {
                    value.source().set_active(true);
                    value
                        .source()
                        .source
                        .cursor_in_pcm_frames()
                        .map_err(AuditoriumError::from)
                }
                _ => unreachable!(),
            }
        })
    }

    /// Sets the volume of this audio source.
    ///
    /// A value of `1.0` represents the source's original volume.
    pub fn set_volume(&self, volume: f32) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .as_node()
                    .set_output_bus_volume(0, volume)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    /// Returns the current volume of this audio source.
    pub fn get_volume(&self) -> HostResult<f32> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => Ok(value.as_node().output_bus_volume(0)),
                _ => unreachable!(),
            }
        })
    }

    /// Returns the length of the audio source in seconds.
    pub fn length_seconds(&self) -> HostResult<f32> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .source()
                    .source
                    .length_in_seconds()
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    /// Returns the length of the audio source in PCM frames.
    pub fn length_pcm(&self) -> HostResult<u64> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .source()
                    .source
                    .length_in_pcm_frames()
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    /// Starts playback of this audio source.
    pub fn start_audio(&self) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => {
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

    /// Stops playback of this audio source.
    pub fn stop_audio(&self) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .as_node()
                    .set_state(NodeState::Stopped)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }
}

impl PcmSource<f32> for TrackedSource<ResourceManagerStream<f32, Unknown>> {
    fn fill_pcm_frames(
        &mut self,
        out: &mut [f32],
        _ctx: &mut maudio::data_source::SourceContext,
    ) -> maudio::MaResult<usize> {
        let frames = self.source.read_pcm_frames_into(out).unwrap_or(0);

        self.set_active(frames != 0);

        Ok(frames)
    }

    fn seek_to_pcm_frame(
        &mut self,
        frame_index: u64,
        ctx: &mut maudio::data_source::SourceContext,
    ) -> maudio::MaResult<()> {
        if self.source.seek_to_pcm_frame(frame_index).is_ok() {
            // Set as inactive we seeked at the end and looping is not enabled
            if frame_index >= self.src_length.unwrap() && !ctx.looping {
                self.set_active(false);
            }
        }
        Ok(())
    }

    fn cursor_in_pcm_frames(&self, _ctx: &maudio::data_source::SourceContext) -> MaResult<u64> {
        self.source.cursor_in_pcm_frames()
    }

    fn length_in_pcm_frames(&self, _ctx: &maudio::data_source::SourceContext) -> MaResult<u64> {
        self.source.length_in_pcm_frames()
    }
}

impl Drop for AudioInner {
    fn drop(&mut self) {
        let id = self.id;
        let store_id = self.store_id;
        let _ = self.call_store_impl(store_id, move |state| {
            let _ = state.mut_nodes().values.remove(&id);
            Ok(())
        });
    }
}
