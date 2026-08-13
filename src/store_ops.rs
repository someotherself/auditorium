use maudio::{audio::sample_rate::SampleRate, engine::node_graph::NodeGraph};

use crate::{
    AuditoriumError, HostResult,
    host::{
        CaptureDeviceId, CaptureDeviceStore, HostState, HostedNodes, NodeId, PlaybackDeviceId,
        PlaybackDeviceStore, Store,
    },
};

/// Trait that allows types implementing [`HostDispatcher`]
/// to check if host is still alive while calling [`HostDispatcher::call`].
pub(crate) trait LiveHost {
    fn is_shutdown(&self) -> bool;
}

pub(crate) trait HostDispatcher: LiveHost {
    fn post<F>(&self, f: F) -> HostResult<()>
    where
        F: FnOnce(&mut HostState) -> HostResult<()> + Send + 'static;

    fn call<F, R>(&self, f: F) -> crate::HostResult<R>
    where
        F: FnOnce(&mut HostState) -> HostResult<R> + Send + 'static,
        R: Send + 'static,
    {
        // TODO: This is still subject to concurrent races
        if self.is_shutdown() {
            return Err(AuditoriumError::HostShutdown);
        }
        let (rtx, rrx) = std::sync::mpsc::channel::<R>();
        self.post(move |state| {
            let r = f(state)?;
            let _ = rtx.send(r);
            Ok(())
        })?;
        let res = rrx.recv()?;
        Ok(res)
    }

    fn call_playback_device<F, R>(&self, device: PlaybackDeviceId, f: F) -> HostResult<R>
    where
        F: FnOnce(&mut PlaybackDeviceStore) -> HostResult<R> + Send + 'static,
        R: Send + 'static,
    {
        self.call(move |state| {
            let store = state.get_playback_device_store(device)?;
            Ok(f(store))
        })?
    }

    fn call_capture_device<F, R>(&self, device: PlaybackDeviceId, f: F) -> HostResult<R>
    where
        F: FnOnce(&mut CaptureDeviceStore) -> HostResult<R> + Send + 'static,
        R: Send + 'static,
    {
        self.call(move |state| {
            let store = state.get_capture_device_store(device)?;
            Ok(f(store))
        })?
    }

    fn call_store_impl<F, R>(&self, device: StoreOrigin, f: F) -> HostResult<R>
    where
        F: FnOnce(&mut dyn StoreOriginImpl) -> HostResult<R> + Send + 'static,
        R: Send + 'static,
    {
        match device {
            StoreOrigin::Playback(id) => self.call(move |state| {
                let store = state.get_playback_device_store(id)?;
                Ok(f(store))
            })?,
            StoreOrigin::Capture(id) => self.call(move |state| {
                let store = state.get_capture_device_store(id)?;
                Ok(f(store))
            })?,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum StoreOrigin {
    Capture(CaptureDeviceId),
    Playback(PlaybackDeviceId),
}
pub(crate) trait StoreOriginImpl {
    fn node_graph(&self) -> &NodeGraph;
    fn nodes(&self) -> &Store<NodeId, HostedNodes>;
    fn mut_nodes(&mut self) -> &mut Store<NodeId, HostedNodes>;
    fn channels(&self) -> u32;
    fn sample_rate(&self) -> SampleRate;
}

impl StoreOriginImpl for CaptureDeviceStore {
    fn node_graph(&self) -> &NodeGraph {
        &self.node_graph
    }

    fn nodes(&self) -> &Store<NodeId, HostedNodes> {
        &self.nodes
    }

    fn mut_nodes(&mut self) -> &mut Store<NodeId, HostedNodes> {
        &mut self.nodes
    }

    fn channels(&self) -> u32 {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }
}

impl StoreOriginImpl for PlaybackDeviceStore {
    fn node_graph(&self) -> &NodeGraph {
        &self.node_graph
    }

    fn nodes(&self) -> &Store<NodeId, HostedNodes> {
        &self.nodes
    }

    fn mut_nodes(&mut self) -> &mut Store<NodeId, HostedNodes> {
        &mut self.nodes
    }

    fn channels(&self) -> u32 {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }
}
