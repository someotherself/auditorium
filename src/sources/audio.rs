use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crossbeam_channel::Sender;
use maudio::{
    MaResult,
    data_source::{
        pcm_source::PcmSource,
        sources::decoder::{DecoderOps, Fs, custom_decoder::CustomDecoder},
    },
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

    pub fn seek_to_pcm_frame(&self, frame: u64) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .source_mut()
                    .source
                    .seek_to_pcm_frame(frame)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    pub fn set_looping(&self, yes: bool) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .source_mut()
                    .source
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
                        .source
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
                        .cursor_pcm()
                        .map_err(AuditoriumError::from)
                }
                _ => unreachable!(),
            }
        })
    }

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

    pub fn length_pcm(&self) -> HostResult<u64> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedDecoder(value) => value
                    .source()
                    .source
                    .length_pcm()
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

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

impl PcmSource<f32> for TrackedSource<CustomDecoder<f32, Fs>> {
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
        self.source.cursor_pcm()
    }

    fn length_in_pcm_frames(&self, _ctx: &maudio::data_source::SourceContext) -> MaResult<u64> {
        self.source.length_pcm()
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
