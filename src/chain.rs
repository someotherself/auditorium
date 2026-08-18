//! Dsp elments and configuration
use std::{
    fmt::Debug,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crossbeam_channel::Sender;
use maudio::{
    audio::sample_rate::SampleRate,
    engine::node_graph::{
        NodeGraphOps,
        nodes::{
            NodeOps, NodeRef,
            effects::delay::DelayNodeBuilder,
            filters::{
                biquad::BiquadNodeBuilder, hishelf::HiShelfNodeBuilder, hpf::HpfNodeBuilder,
                loshelf::LoShelfNodeBuilder, lpf::LpfNodeBuilder, notch::NotchNodeBuilder,
                peak::PeakNodeBuilder,
            },
        },
    },
};

use crate::{
    AuditoriumError, HostResult,
    device::CaptureDevice,
    host::{DspId, HostCommand, HostedNodes, NodeId},
    sources::{audio::Audio, noise::Noise, pulse::Pulse, wave::Wave},
    store_ops::{HostDispatcher, LiveHost, StoreOrigin},
};

pub trait Dsp {
    // Internal. TODO: Hide in the future
    fn dst_target<'a>(&'a self) -> DspTarget<'a>;
    // Internal. TODO: Hide in the future
    fn splitter(&self, out_bus_count: u32) -> HostResult<NodeId>;

    /// Start a new dsp chain on this source
    fn dsp<'a>(&'a self) -> DspChain<'a> {
        DspChain::apply_chain(DspChain::new(), self.dst_target())
    }

    /// Add a splitter, starting `N` number of new dsp chains
    ///
    /// All branches of the splitter will be mixed on the output
    fn dsp_split<'a, const N: usize>(&'a self) -> HostResult<[DspChain<'a>; N]> {
        let mut chain = [DspChain::apply_chain(DspChain::new(), self.dst_target()); N];
        let splitter = self.splitter(N as u32)?;
        for (idx, el) in chain.iter_mut().enumerate() {
            el.splitter = Some(Splitter {
                id: splitter,
                bus_index: idx as u32,
            })
        }
        Ok(chain)
    }

    /// Apply an existing dsp chain to this source
    fn apply_chain(&self, chain: DspChain) -> HostResult<ConnectedChain> {
        let chain = DspChain::apply_chain(chain, self.dst_target());
        chain.connect()
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum DspElement {
    Hpf {
        frequency: f64,
        order: u32,
    },
    Lpf {
        frequency: f64,
        order: u32,
    },
    Biquad {
        b0: f32,
        b1: f32,
        b2: f32,
        a0: f32,
        a1: f32,
        a2: f32,
    },
    HiShelf {
        gain_db: f64,
        shelf_slope: f64,
        frequency: f64,
    },
    LoShelf {
        gain_db: f64,
        shelf_slope: f64,
        frequency: f64,
    },
    Notch {
        quality_factor: f64,
        frequency: f64,
    },
    Peak {
        gain_db: f64,
        q: f64,
        frequency: f64,
    },
    Delay {
        delay_frames: u32,
        decay: f32,
        wet: f32,
        dry: f32,
    },
    None,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Splitter {
    id: NodeId,
    bus_index: u32,
}

#[doc(hidden)]
#[derive(Clone, Copy)]
pub enum DspTarget<'a> {
    None,
    Audio(&'a Audio),
    Pulse(&'a Pulse),
    Wave(&'a Wave),
    Noise(&'a Noise),
    Capture(&'a CaptureDevice),
}

impl Debug for DspTarget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "None",
            Self::Audio(_) => "Audio",
            Self::Pulse(_) => "Pulse",
            Self::Wave(_) => "Wave",
            Self::Noise(_) => "Noise",
            Self::Capture(_) => "Capture",
        })
    }
}

/// A configuration for a dsp chain that can be applied to a source
///
/// [`DspChain::connect`] must be used for this chain to be connected,
/// creating a [`ConnectedChain`]
#[derive(Clone, Copy, Debug)]
pub struct DspChain<'a> {
    elements: [DspElement; 32],
    length: usize,
    target: DspTarget<'a>,
    splitter: Option<Splitter>,
}

/// A chain of dsp nodes in a node graph
///
/// When this chain is dropped, the dsp nodes are disconnected and dropped,
/// therefore, it needs to be kept alive
pub struct ConnectedChain {
    store_id: StoreOrigin,
    elements: ConnectedElements,
    sender: Sender<HostCommand>,
    is_shutdown: Arc<AtomicBool>,
}

struct ConnectedElements {
    elements: [DspId; 32],
    nodes: [DspElement; 32],
    length: usize,
}

impl std::fmt::Display for ConnectedChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (idx, node) in self.elements.nodes.iter().enumerate() {
            if idx >= self.elements.length {
                break;
            };
            if idx > 0 {
                writeln!(f)?;
            }
            write!(f, "{:?}", node)?;
        }
        Ok(())
    }
}

impl ConnectedElements {
    fn new(nodes: [DspElement; 32]) -> Self {
        Self {
            elements: [0; 32],
            nodes,
            length: 0,
        }
    }

    fn add(&mut self, id: DspId) {
        self.elements[self.length] = id;
        self.length += 1;
    }
}

unsafe impl Send for ConnectedChain {}

// For now, this only works because splitters
// can't be added or other sounds can't be mixed
// in the middle of the chain
impl Drop for ConnectedChain {
    fn drop(&mut self) {
        let id = self.store_id;
        let elements = self.elements.elements;
        let length = self.elements.length;
        let _ = self.call_store_impl(id, move |store| {
            for (idx, dsp) in elements.iter().enumerate() {
                if idx >= length {
                    break;
                }
                store.mut_nodes().values.remove(dsp);
            }
            Ok(())
        });
    }
}

impl LiveHost for ConnectedChain {
    fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::Relaxed)
    }
}

impl HostDispatcher for ConnectedChain {
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

type OutBus = u32;

#[derive(Copy, Clone)]
struct Output((NodeId, OutBus));

impl Output {
    fn out_bus(&self) -> OutBus {
        self.0.1
    }

    fn id(&self) -> NodeId {
        self.0.0
    }
}

impl<'a> DspChain<'a> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            elements: [DspElement::None; 32],
            length: 0,
            target: DspTarget::None,
            splitter: None,
        }
    }

    pub(crate) fn apply_chain(mut chain: DspChain<'a>, target: DspTarget<'a>) -> Self {
        chain.target = target;
        chain
    }

    fn build_node<N: maudio::engine::node_graph::AsNodeGraphPtr>(
        node_graph: &N,
        channels: u32,
        sample_rate: SampleRate,
        element: DspElement,
    ) -> HostResult<HostedNodes> {
        match element {
            DspElement::Biquad {
                b0,
                b1,
                b2,
                a0,
                a1,
                a2,
            } => {
                let node =
                    BiquadNodeBuilder::new(node_graph, channels, b0, b1, b2, a0, a1, a2).build()?;
                Ok(HostedNodes::BiquadNode(node))
            }
            DspElement::HiShelf {
                gain_db,
                shelf_slope,
                frequency,
            } => {
                let node = HiShelfNodeBuilder::new(
                    node_graph,
                    channels,
                    sample_rate,
                    gain_db,
                    shelf_slope,
                    frequency,
                )
                .build()?;
                Ok(HostedNodes::HiShelfNode(node))
            }
            DspElement::LoShelf {
                gain_db,
                shelf_slope,
                frequency,
            } => {
                let node = LoShelfNodeBuilder::new(
                    node_graph,
                    channels,
                    sample_rate,
                    gain_db,
                    shelf_slope,
                    frequency,
                )
                .build()?;
                Ok(HostedNodes::LoShelfNode(node))
            }
            DspElement::Hpf { frequency, order } => {
                let node = HpfNodeBuilder::new(node_graph, channels, sample_rate, frequency, order)
                    .build()?;
                Ok(HostedNodes::HpfNode(node))
            }
            DspElement::Lpf { frequency, order } => {
                let node = LpfNodeBuilder::new(node_graph, channels, sample_rate, frequency, order)
                    .build()?;
                Ok(HostedNodes::LpfNode(node))
            }
            DspElement::Peak {
                gain_db,
                q,
                frequency,
            } => {
                let node =
                    PeakNodeBuilder::new(node_graph, channels, sample_rate, gain_db, q, frequency)
                        .build()?;
                Ok(HostedNodes::PeakNode(node))
            }
            DspElement::Notch {
                quality_factor,
                frequency,
            } => {
                let node = NotchNodeBuilder::new(
                    node_graph,
                    channels,
                    sample_rate,
                    quality_factor,
                    frequency,
                )
                .build()?;
                Ok(HostedNodes::NotchNode(node))
            }
            DspElement::Delay {
                delay_frames,
                decay,
                wet,
                dry,
            } => {
                let node =
                    DelayNodeBuilder::new(node_graph, channels, sample_rate, delay_frames, decay)
                        .dry(dry)
                        .wet(wet)
                        .build()?;
                Ok(HostedNodes::DelayNode(node))
            }
            DspElement::None => Err(AuditoriumError::InvalidDevice),
        }
    }

    fn match_node(node: &HostedNodes) -> NodeRef<'_> {
        match node {
            HostedNodes::BiquadNode(node) => node.as_node(),
            HostedNodes::HiShelfNode(node) => node.as_node(),
            HostedNodes::LoShelfNode(node) => node.as_node(),
            HostedNodes::HpfNode(node) => node.as_node(),
            HostedNodes::LpfNode(node) => node.as_node(),
            HostedNodes::PeakNode(node) => node.as_node(),
            HostedNodes::NotchNode(node) => node.as_node(),
            HostedNodes::DelayNode(node) => node.as_node(),
            HostedNodes::AttachedDecoder(node) => node.as_node(),
            HostedNodes::AttachedWave(node) => node.as_node(),
            HostedNodes::AttachedPulse(node) => node.as_node(),
            HostedNodes::AttachedNoise(node) => node.as_node(),
            HostedNodes::SplitterNode(node) => node.as_node(),
        }
    }

    fn connect_source<H: HostDispatcher>(
        &self,
        src: &H,
        store_id: StoreOrigin,
        src_id: u64,
        splitter: Option<Splitter>,
        elements: [DspElement; 32],
    ) -> HostResult<ConnectedElements> {
        let chain = src.call_store_impl(store_id, move |state| {
            let mut dsp_elements = ConnectedElements::new(elements);
            let mut start = Output((src_id, 0)); // start of the dsp chain. Either the source or the splitter.
            let mut end = start; // last known node before the endpoint

            // Check if we have a splitter
            if let Some(splitter) = splitter {
                let src_node = state.nodes().values.get(&start.id()).unwrap();
                let splitter_node = state.nodes().values.get(&splitter.id).unwrap();
                Self::match_node(src_node).attach_output_bus(
                    start.out_bus(),
                    &mut Self::match_node(splitter_node),
                    0,
                )?;
                start = Output((splitter.id, splitter.bus_index));
                end = start;
            };

            // Go through the actual DSP nodes now
            for (idx, &el) in elements.iter().enumerate() {
                if matches!(el, DspElement::None) {
                    break;
                }
                let dsp_node: HostedNodes = Self::build_node(
                    state.node_graph(),
                    state.channels(),
                    state.sample_rate(),
                    el,
                )?;

                // out is the previous element we must connect to
                let mut out = end;
                if idx == 0 {
                    out = start
                };
                let prev_node = state.nodes().values.get(&out.id()).unwrap();
                Self::match_node(prev_node).attach_output_bus(
                    out.out_bus(),
                    &mut Self::match_node(&dsp_node),
                    0,
                )?;

                let curr_node_id = state.mut_nodes().insert(dsp_node);
                dsp_elements.add(curr_node_id);
                end = Output((curr_node_id, 0))
            }

            // We have reched the end of the chain. Connect to the endpoint
            let mut endpoint = state.node_graph().endpoint();
            let end_node = state.nodes().values.get(&end.0.0).unwrap();
            Self::match_node(end_node).attach_output_bus(end.0.1, &mut endpoint, 0)?;
            Ok(dsp_elements)
        })?;
        Ok(chain)
    }

    fn connect_capture_chain(
        &self,
        device: &CaptureDevice,
        elements: [DspElement; 32],
    ) -> HostResult<ConnectedElements> {
        let device_id = device.0.device;
        let splitter = self.splitter;
        let chain = device.call_capture_device(device_id, move |state| {
            let mut dsp_elements = ConnectedElements::new(elements);
            let node_graph = &state.node_graph;
            let mut start = Output((state.device_node, 0)); // start of the dsp chain. Either the source or the splitter.
            let mut end = start; // last known node before the endpoint

            // Check if we have a spliter
            if let Some(splitter) = splitter {
                let dev_node = state.nodes.values.get(&start.id()).unwrap();
                let splitter_node = state.nodes.values.get(&splitter.id).unwrap();
                Self::match_node(dev_node).attach_output_bus(
                    start.out_bus(),
                    &mut Self::match_node(splitter_node),
                    0,
                )?;
                start = Output((splitter.id, splitter.bus_index));
                end = start;
            };

            // Go through the actual DSP nodes now
            for (idx, &el) in elements.iter().enumerate() {
                if matches!(el, DspElement::None) {
                    break;
                }
                let dsp_node: HostedNodes =
                    Self::build_node(node_graph, state.channels, state.sample_rate, el)?;

                // out is the previous element we must connect to
                let mut out = end;
                if idx == 0 {
                    out = start
                };

                let prev_node = state.nodes.values.get(&out.id()).unwrap();
                Self::match_node(prev_node).attach_output_bus(
                    out.out_bus(),
                    &mut Self::match_node(&dsp_node),
                    0,
                )?;

                let curr_node_id = state.nodes.insert(dsp_node);
                dsp_elements.add(curr_node_id);
                end = Output((curr_node_id, 0))
            }

            // We have reched the end. Connect to the endpoint
            let mut endpoint = node_graph.endpoint();
            let end_node = state.nodes.values.get(&end.id()).unwrap();
            Self::match_node(end_node).attach_output_bus(end.out_bus(), &mut endpoint, 0)?;

            Ok(dsp_elements)
        })?;
        Ok(chain)
    }

    /// A dsp chain must be connected before it can be used
    ///
    /// This returns a `ConnectedChain` which must be kept alive or the audio source
    /// will be disconnected from the output
    pub fn connect(&self) -> HostResult<ConnectedChain> {
        let elements: [DspElement; 32] = self.elements;

        let connected_dsp = match self.target {
            DspTarget::None => return Err(AuditoriumError::DanglingChain),
            DspTarget::Audio(audio) => {
                let elements = self.connect_source(
                    audio.0.clone().as_ref(),
                    audio.0.store_id,
                    audio.0.id,
                    self.splitter,
                    elements,
                )?;
                ConnectedChain {
                    store_id: audio.0.store_id,
                    elements,
                    sender: audio.0.sender.clone(),
                    is_shutdown: audio.0.is_shutdown.clone(),
                }
            }
            DspTarget::Capture(device) => {
                let elements = self.connect_capture_chain(device, elements)?;
                ConnectedChain {
                    store_id: StoreOrigin::Capture(device.0.device),
                    elements,
                    sender: device.0.sender.clone(),
                    is_shutdown: device.0.is_shutdown.clone(),
                }
            }
            DspTarget::Pulse(pulse) => {
                let elements = self.connect_source(
                    pulse.0.clone().as_ref(),
                    pulse.0.store_id,
                    pulse.0.id,
                    self.splitter,
                    elements,
                )?;
                ConnectedChain {
                    store_id: pulse.0.store_id,
                    elements,
                    sender: pulse.0.sender.clone(),
                    is_shutdown: pulse.0.is_shutdown.clone(),
                }
            }
            DspTarget::Wave(wave) => {
                let elements = self.connect_source(
                    wave.0.clone().as_ref(),
                    wave.0.store_id,
                    wave.0.id,
                    self.splitter,
                    elements,
                )?;
                ConnectedChain {
                    store_id: wave.0.store_id,
                    elements,
                    sender: wave.0.sender.clone(),
                    is_shutdown: wave.0.is_shutdown.clone(),
                }
            }
            DspTarget::Noise(noise) => {
                let elements = self.connect_source(
                    noise.0.clone().as_ref(),
                    noise.0.store_id,
                    noise.0.id,
                    self.splitter,
                    elements,
                )?;
                ConnectedChain {
                    store_id: noise.0.store_id,
                    elements,
                    sender: noise.0.sender.clone(),
                    is_shutdown: noise.0.is_shutdown.clone(),
                }
            }
        };

        Ok(connected_dsp)
    }

    #[inline]
    fn add_link(&mut self, link: DspElement) -> &mut Self {
        self.elements[self.length] = link;
        if self.length + 1 < 32 {
            // TODO: Return an error when building the chain?
            self.length += 1;
        }
        self
    }

    #[must_use = "the DSP chain must eventually be connected with `.connect()`"]
    pub fn hpf(&mut self, frequency: f64, order: u32) -> &mut Self {
        let link = DspElement::Hpf { frequency, order };
        self.add_link(link)
    }

    #[must_use = "the DSP chain must eventually be connected with `.connect()`"]
    pub fn lpf(&mut self, frequency: f64, order: u32) -> &mut Self {
        let link = DspElement::Lpf { frequency, order };
        self.add_link(link)
    }

    #[must_use = "the DSP chain must eventually be connected with `.connect()`"]
    pub fn biquad(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> &mut Self {
        let link = DspElement::Biquad {
            b0,
            b1,
            b2,
            a0,
            a1,
            a2,
        };
        self.add_link(link);
        self
    }

    #[must_use = "the DSP chain must eventually be connected with `.connect()`"]
    pub fn hishelf(&mut self, gain_db: f64, shelf_slope: f64, frequency: f64) -> &mut Self {
        let link = DspElement::HiShelf {
            gain_db,
            shelf_slope,
            frequency,
        };
        self.add_link(link);
        self
    }

    #[must_use = "the DSP chain must eventually be connected with `.connect()`"]
    pub fn loshelf(&mut self, gain_db: f64, shelf_slope: f64, frequency: f64) -> &mut Self {
        let link = DspElement::LoShelf {
            gain_db,
            shelf_slope,
            frequency,
        };
        self.add_link(link);
        self
    }

    #[must_use = "the DSP chain must eventually be connected with `.connect()`"]
    pub fn notch(&mut self, quality_factor: f64, frequency: f64) -> &mut Self {
        let link = DspElement::Notch {
            quality_factor,
            frequency,
        };
        self.add_link(link);
        self
    }

    #[must_use = "the DSP chain must eventually be connected with `.connect()`"]
    pub fn peak(&mut self, gain_db: f64, q: f64, frequency: f64) -> &mut Self {
        let link = DspElement::Peak {
            gain_db,
            q,
            frequency,
        };
        self.add_link(link);
        self
    }

    #[must_use = "the DSP chain must eventually be connected with `.connect()`"]
    pub fn delay(&mut self, delay_frames: u32, decay: f32, wet: f32, dry: f32) -> &mut Self {
        let link = DspElement::Delay {
            delay_frames,
            decay,
            wet,
            dry,
        };
        self.add_link(link);
        self
    }
}
