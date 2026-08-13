use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crossbeam_channel::Sender;
use maudio::{
    MaResult,
    data_source::{
        pcm_source::PcmSource,
        sources::pulsewave::{PulseWave, PulseWaveOps},
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
pub struct Pulse(pub(crate) Arc<PulseInner>);

pub(crate) struct PulseInner {
    pub(crate) sender: Sender<HostCommand>,
    pub(crate) store_id: StoreOrigin,
    pub(crate) id: SourceId,
    pub(crate) is_shutdown: Arc<AtomicBool>,
}

impl LiveHost for PulseInner {
    fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for PulseInner {
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

impl Dsp for Pulse {
    fn dst_target<'a>(&'a self) -> DspTarget<'a> {
        DspTarget::Pulse(self)
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

impl Pulse {
    pub(crate) fn new(
        id: SourceId,
        store_id: StoreOrigin,
        sender: Sender<HostCommand>,
        flag: Arc<AtomicBool>,
    ) -> Self {
        Pulse(Arc::new(PulseInner {
            sender,
            store_id,
            id,
            is_shutdown: flag,
        }))
    }

    pub fn start_audio(&self) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedPulse(value) => {
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

    pub fn set_volume(&self, volume: f32) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedPulse(value) => value
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
                HostedNodes::AttachedPulse(value) => Ok(value.as_node().output_bus_volume(0)),
                _ => unreachable!(),
            }
        })
    }

    pub fn stop_audio(&self) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedPulse(value) => value
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
                HostedNodes::AttachedPulse(value) => value
                    .source_mut()
                    .inner_source_mut()
                    .set_amplitude(amplitude)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    pub fn set_frequency(&self, frequency: f64) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedPulse(value) => value
                    .source_mut()
                    .inner_source_mut()
                    .set_frequency(frequency)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }

    pub fn set_duty_cycle(&self, duty_cycle: f64) -> HostResult<()> {
        let id = self.0.id;
        let device_id = self.0.store_id;
        self.0.call_store_impl(device_id, move |store| {
            match store.mut_nodes().values.get_mut(&id).unwrap() {
                HostedNodes::AttachedPulse(value) => value
                    .source_mut()
                    .inner_source_mut()
                    .set_duty_cycle(duty_cycle)
                    .map_err(AuditoriumError::from),
                _ => unreachable!(),
            }
        })
    }
}

impl PcmSource<f32> for TrackedSource<PulseWave<f32>> {
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

impl Drop for PulseInner {
    fn drop(&mut self) {
        let id = self.id;
        let store_id = self.store_id;
        let _ = self.call_store_impl(store_id, move |state| {
            let _ = state.mut_nodes().values.remove(&id);
            Ok(())
        });
    }
}
