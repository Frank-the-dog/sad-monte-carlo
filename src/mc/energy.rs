//! An implementation of SAD (Statistical Association, Dynamical version).

#![allow(non_snake_case)]

use ::system::*;
use super::*;

use super::plugin::Plugin;
use dimensioned::Dimensionless;
use rand::Rng;
use std::default::Default;

/// Parameters to configure a particular MC.
#[derive(Debug, ClapMe)]
pub enum MethodParams {
    /// Sad
    Sad {
        /// The minimum temperature we are interested in.
        min_T: Energy,
    },
    /// Samc
    Samc {
        /// The t0 parameter, determining how long to leave gamma=1.
        t0: u64,
    },
}

/// The parameters needed to configure a simulation.
#[derive(Debug, ClapMe)]
pub struct EnergyMCParams {
    /// The actual method.
    pub _method: MethodParams,
    /// The seed for the random number generator.
    pub seed: Option<u64>,
    _maxiter: plugin::MaxIterParams,
    _final_report: plugin::FinalReportParams,
}

impl Default for EnergyMCParams {
    fn default() -> Self {
        EnergyMCParams {
            _method: MethodParams::Sad { min_T: 0.2*units::EPSILON },
            seed: None,
            _maxiter: plugin::MaxIterParams::default(),
            _final_report: plugin::FinalReportParams::default(),
        }
    }
}

/// A square well fluid.
#[derive(Serialize, Deserialize, Debug)]
pub struct EnergyMC<S> {
    /// The system we are simulating.
    pub system: S,
    /// The method we use
    method: Method,
    /// The number of moves that have been made.
    pub moves: u64,
    /// The last move where we discovered a new energy.
    pub time_L: u64,
    /// The number of moves that have been rejected.
    pub rejected_moves: u64,
    /// The number of times we have been at each energy.
    pub histogram: Vec<u64>,
    /// The ln weight for each energy bin.
    pub lnw: Vec<Unitless>,
    /// The lowest allowed energy in any bin.
    pub min_energy_bin: Energy,
    /// The energy bin size.
    pub energy_bin: Energy,
    /// The max-entropy energy.
    pub max_entropy_energy: Energy,
    /// The max-entropy energy.
    pub max_S: Unitless,


    /// The random number generator.
    pub rng: ::rng::MyRng,
    /// Where to save the resume file.
    pub save_as: ::std::path::PathBuf,
    maxiter: plugin::MaxIter,
    final_report: plugin::FinalReport,
    manager: plugin::PluginManager,
}

#[derive(Serialize, Deserialize, Debug)]
enum Method {
    /// Sad
    Sad {
        min_T: Energy,
        too_lo: Energy,
        too_hi: Energy,
        min_important_energy: Energy,
        tL: u64,
        n_found: u64,
    },
    /// Samc
    Samc {
        t0: u64,
    },
}

impl Method {
    fn new(p: MethodParams, E: Energy) -> Self {
        match p {
            MethodParams::Sad { min_T } =>
                Method::Sad {
                    min_T,
                    too_lo: E,
                    too_hi: E,
                    min_important_energy: E,
                    tL: 0,
                    n_found: 1,
                },
            MethodParams::Samc { t0 } => Method::Samc { t0 },
        }
    }
}

impl<S: System> EnergyMC<S> {
    /// Find the index corresponding to a given energy.  This should
    /// panic if the energy is less than `min_energy_bin`.
    pub fn energy_to_index(&self, e: Energy) -> usize {
        *((e - self.min_energy_bin)/self.energy_bin).value() as usize
    }
    /// Find the energy corresponding to a given index.
    pub fn index_to_energy(&self, i: usize) -> Energy {
        self.min_energy_bin + (i as f64)*self.energy_bin
    }
    /// Make room in our arrays for a new energy value
    pub fn prepare_for_energy(&mut self, e: Energy) {
        assert!(self.energy_bin > Energy::new(0.0));
        while e < self.min_energy_bin {
            // this is a little wasteful, but seems the easiest way to
            // ensure we end up with enough room.
            self.histogram.insert(0, 0);
            self.lnw.insert(0, Unitless::new(0.0));
            self.min_energy_bin -= self.energy_bin;
        }
        while e >= self.min_energy_bin + self.energy_bin*(self.lnw.len() as f64) {
            self.lnw.push(Unitless::new(0.0));
            self.histogram.push(0);
        }
    }

    /// This decides whether to reject the move based on the actual
    /// method in use.
    fn reject_move(&mut self, e1: Energy, e2: Energy) -> bool {
        let i1 = self.energy_to_index(e1);
        let i2 = self.energy_to_index(e2);
        match self.method {
            Method::Sad { too_lo, too_hi,  .. } => {
                let lnw1 = if e1 < too_lo {
                    self.lnw[self.energy_to_index(too_lo)].value()
                } else if e1 > too_hi {
                    self.lnw[self.energy_to_index(too_hi)].value()
                } else {
                    self.lnw[i1].value()
                };
                let lnw2 = if e2 < too_lo {
                    self.lnw[self.energy_to_index(too_lo)].value()
                } else if e2 > too_hi {
                    self.lnw[self.energy_to_index(too_hi)].value()
                } else {
                    self.lnw[i2].value()
                };
                let rejected = lnw2 > lnw1 && self.rng.gen::<f64>() > (lnw1 - lnw2).exp();
                if !rejected && self.histogram[i2] == 0 {
                    // Here we do changes that need only happen when
                    // we encounter an energy we have never seen before.
                    match self.method {
                        Method::Sad { ref mut n_found, ref mut tL, .. } => {
                            *n_found += 1;
                            *tL = self.moves;
                        }
                        _ => unreachable!()
                    }
                }
                rejected
            }
            Method::Samc { .. } => {
                let lnw1 = self.lnw[i1].value();
                let lnw2 = self.lnw[i2].value();
                lnw2 > lnw1 && self.rng.gen::<f64>() > (lnw1 - lnw2).exp()
            }
        }
    }
    /// This updates the lnw based on the actual method in use.
    fn update_weights(&mut self, energy: Energy) {
        let i = self.energy_to_index(energy);
        match self.method {
            Method::Sad { too_lo, too_hi, min_important_energy, min_T, n_found, tL, .. } => {
                if too_lo < too_hi {
                    let t = self.moves as f64;
                    let tL = tL as f64;
                    let dE = too_hi - too_lo;
                    let n = n_found as f64;

                    let gamma = dE/(3.0*min_T*t)*(n*n + t*(t/tL - 1.0) + n*t)/(n*n + t*(t/tL - 1.0) + t);
                    if energy < too_lo || energy > too_hi {
                        // We are at higher energy than the maximum
                        // entropy state, so we need to tweak our
                        // weights by even more, since we don't spend
                        // much time here.

                        // We key our change in weights based on the max_entropy_state.
                        // 1/w = 1/w + gamma 1/w0
                        // -lnw = ln(1/w + gamma 1/w0) = ln((w0/w + gamma)/w0)
                        //      = -lnw0 + ln(w0/w + gamma) = -lnw0 + ln(gamma + exp(lnw0-lnw))
                        // lnw = lnw0 - ln(gamma + exp(lnw0-lnw))
                        let lnw = self.lnw[i];
                        let lnw0 = if energy > too_hi {
                            self.lnw[self.energy_to_index(too_hi)]
                        } else {
                            self.lnw[self.energy_to_index(too_lo)]
                        };
                        if lnw0 > lnw {
                            // If w0 > w then we can turn into logs like so:
                            // lnw = ln((w/w0 + gamma)*w0)
                            //     = lnw0 + ln(w/w0 + gamma) = lnw0 + ln(gamma + exp(lnw-lnw0))
                            // lnw = lnw0 + ln(gamma + exp(lnw-lnw0))
                            self.lnw[i] = lnw0 + log((exp(gamma)-1.) + exp(lnw - lnw0));
                        } else {
                            // If w > w0 then we can turn into logs like so:
                            // lnw = ln((1 + gamma*w0/w)*w)
                            //     = lnw + ln(1 + gamma*w0/w) = lnw + ln(1 + gamma exp(lnw0-lnw))
                            // lnw = lnw + ln(1 + gamma exp(lnw0-lnw))
                            self.lnw[i] = lnw + log(1.0 + (exp(gamma)-1.)*exp(lnw0 - lnw));
                        }
                    } else {
                        // We are in the "interesting" region, so use an ordinary SA update.
                        self.lnw[i] += gamma;
                    }
                }

                if self.lnw[i] > self.max_S && energy > too_hi {
                    match self.method {
                        Method::Sad { ref mut too_hi, .. } => {
                            *too_hi = energy;
                        }
                        _ => unreachable!()
                    }
                }
                let boltz = self.lnw[self.energy_to_index(min_important_energy)] + min_important_energy/min_T;
                if self.lnw[i] + energy/min_T > boltz {
                    match self.method {
                        Method::Sad { ref mut too_lo, ref mut min_important_energy, .. } => {
                            *min_important_energy = energy;
                            if energy < *too_lo {
                                *too_lo = energy;
                            }
                        }
                        _ => unreachable!()
                    }
                }
            }
            Method::Samc { t0 } => {
                let t = self.moves;
                self.lnw[i] += if t > t0 { t0 as f64/t as f64 } else { 1.0 };
            }
        }
    }
}

impl<S: MovableSystem> MonteCarlo for EnergyMC<S> {
    type Params = EnergyMCParams;
    type System = S;
    fn from_params(params: EnergyMCParams, system: S, save_as: ::std::path::PathBuf) -> Self {
        EnergyMC {
            method: Method::new(params._method, system.energy()),
            moves: 0,
            time_L: 0,
            rejected_moves: 0,
            histogram: vec![1],
            lnw: vec![Unitless::new(0.0)],
            min_energy_bin: system.energy(),
            max_entropy_energy: system.energy(),
            max_S: Unitless::new(0.0),
            energy_bin: system.delta_energy().unwrap_or(Energy::new(1.0)),
            system: system,

            rng: ::rng::MyRng::from_u64(params.seed.unwrap_or(0)),
            save_as: save_as,
            maxiter: plugin::MaxIter::from(params._maxiter),
            final_report: plugin::FinalReport::from(params._final_report),
            manager: plugin::PluginManager::new(),
        }
    }

    fn move_once(&mut self) {
        self.moves += 1;
        let e1 = self.system.energy();
        if let Some(_) = self.system.move_once(&mut self.rng, Length::new(0.1)) {
            let e2 = self.system.energy();
            self.prepare_for_energy(e2);

            if self.reject_move(e1,e2) {
                self.system.undo();
                self.rejected_moves += 1;
            }
        } else {
            // The system itself rejected the move.
            self.rejected_moves += 1;
        }
        let energy = self.system.energy();
        let i = self.energy_to_index(energy);

        self.histogram[i] += 1;
        self.update_weights(e1);

        if self.lnw[i] > self.max_S {
            self.max_S = self.lnw[i];
            self.max_entropy_energy = energy;
        }
        let plugins = [&self.maxiter as &Plugin<Self>,
                       &self.final_report,
        ];
        self.manager.run(self, &self.system, &plugins);
    }
    fn system(&self) -> &Self::System {
        &self.system
    }
    fn num_moves(&self) -> u64 {
        self.moves
    }
    fn num_rejected_moves(&self) -> u64 {
        self.rejected_moves
    }
    fn save_as(&self) -> ::std::path::PathBuf {
        self.save_as.clone()
    }
}

fn log(x: Unitless) -> Unitless {
    Unitless::new(x.ln())
}
fn exp(x: Unitless) -> Unitless {
    Unitless::new(x.exp())
}
