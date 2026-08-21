use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

use maudio::{
    audio::{sample_rate::SampleRate, wave_shape::WaveFormType},
    data_source::{
        DataSource, DataSourceOps,
        data_source_builder::DataSourceBuilder,
        sources::{
            noise::{Noise, NoiseBuilder, NoiseType},
            pulsewave::{PulseWave, PulseWaveBuilder},
            waveform::{WaveForm, WaveFormBuilder},
        },
    },
    engine::resource::{Unknown, rm_stream::ResourceManagerStream},
};

use crate::{AuditoriumError, HostResult};

pub(crate) struct PlaybackActivity {
    pub(crate) tracker: AtomicU32,
    pub(crate) user_flag: Option<Arc<AtomicBool>>,
}

impl PlaybackActivity {
    pub(crate) fn new(flag: Option<Arc<AtomicBool>>) -> Rc<Self> {
        Rc::new(PlaybackActivity {
            tracker: AtomicU32::new(0),
            user_flag: flag,
        })
    }
}

pub(crate) struct TrackedSource<S> {
    active_players: Rc<PlaybackActivity>,
    pub(crate) is_active: AtomicBool,
    pub(crate) src_length: Option<u64>,
    pub(crate) source: S,
}

impl<S> TrackedSource<S> {
    pub(crate) fn set_active(&self, yes: bool) {
        let old = self.is_active.swap(yes, Ordering::Relaxed);
        if old != yes {
            if yes {
                let prev = self.active_players.tracker.fetch_add(1, Ordering::Relaxed);
                if let Some(ref flag) = self.active_players.user_flag
                    && prev == 0
                {
                    flag.store(true, Ordering::Relaxed);
                }
            } else {
                let prev = self.active_players.tracker.fetch_sub(1, Ordering::Relaxed);
                if let Some(ref flag) = self.active_players.user_flag
                    && prev == 1
                {
                    flag.store(false, Ordering::Relaxed);
                }
            }
        }
    }
}

impl<S> TrackedSource<S> {
    pub(crate) fn inner_source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    pub(crate) fn new_decoder(
        channels: u32,
        sample_rate: SampleRate,
        tracker: Rc<PlaybackActivity>,
        stream: ResourceManagerStream<f32, Unknown>,
    ) -> HostResult<DataSource<f32, TrackedSource<ResourceManagerStream<f32, Unknown>>>> {
        let len = stream.length_in_pcm_frames()?;
        let src = TrackedSource {
            active_players: tracker,
            is_active: AtomicBool::new(false),
            src_length: Some(len),
            source: stream,
        };
        let ds = DataSourceBuilder::new(channels, sample_rate)
            .build_f32::<TrackedSource<ResourceManagerStream<f32, Unknown>>>(src)
            .map_err(AuditoriumError::from)?;
        Ok(ds)
    }

    pub(crate) fn new_wave(
        channels: u32,
        sample_rate: SampleRate,
        wave_type: WaveFormType,
        amplitude: f64,
        frequency: f64,
        tracker: Rc<PlaybackActivity>,
    ) -> HostResult<DataSource<f32, TrackedSource<WaveForm<f32>>>> {
        let wave = WaveFormBuilder::new(channels, sample_rate, wave_type, amplitude, frequency)
            .build_f32()?;
        let src = TrackedSource {
            active_players: tracker,
            is_active: AtomicBool::new(false),
            src_length: None,
            source: wave,
        };
        DataSourceBuilder::new(channels, sample_rate)
            .build_f32::<TrackedSource<WaveForm<f32>>>(src)
            .map_err(AuditoriumError::from)
    }

    pub(crate) fn new_pulse(
        channels: u32,
        sample_rate: SampleRate,
        amplitude: f64,
        frequency: f64,
        duty_cycle: f64,
        tracker: Rc<PlaybackActivity>,
    ) -> HostResult<DataSource<f32, TrackedSource<PulseWave<f32>>>> {
        let pulse = PulseWaveBuilder::new(channels, sample_rate, amplitude, frequency, duty_cycle)
            .build_f32()?;
        let src = TrackedSource {
            active_players: tracker,
            is_active: AtomicBool::new(false),
            src_length: None,
            source: pulse,
        };
        DataSourceBuilder::new(channels, sample_rate)
            .build_f32::<TrackedSource<PulseWave<f32>>>(src)
            .map_err(AuditoriumError::from)
    }

    pub(crate) fn new_noise(
        channels: u32,
        sample_rate: SampleRate,
        amplitude: f64,
        noise_type: NoiseType,
        tracker: Rc<PlaybackActivity>,
    ) -> HostResult<DataSource<f32, TrackedSource<Noise<f32>>>> {
        let noise = NoiseBuilder::new(channels, noise_type, amplitude).build_f32()?;
        let src = TrackedSource {
            active_players: tracker,
            is_active: AtomicBool::new(false),
            src_length: None,
            source: noise,
        };
        DataSourceBuilder::new(channels, sample_rate)
            .build_f32::<TrackedSource<Noise<f32>>>(src)
            .map_err(AuditoriumError::from)
    }
}
