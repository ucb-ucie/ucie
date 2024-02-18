use atoll::route::{GreedyRouter, ViaMaker};
use atoll::{IoBuilder, Tile, TileBuilder};
use serde::{Deserialize, Serialize};
use sky130pdk::atoll::{MosLength, NmosTile, NtapTile, PmosTile, PtapTile, Sky130ViaMaker};
use sky130pdk::Sky130Pdk;
use std::any::Any;
use std::marker::PhantomData;
use std::ops::Deref;
use substrate::arcstr::ArcStr;
use substrate::block::Block;
use substrate::error::Result;
use substrate::geometry::align::AlignMode;
use substrate::geometry::bbox::Bbox;
use substrate::geometry::dir::Dir;
use substrate::geometry::rect::Rect;
use substrate::geometry::span::Span;
use substrate::io::layout::{Builder, IoShape};
use substrate::io::schematic::{Bundle, Node};
use substrate::io::{DiffPair, InOut, Input, Io, MosIo, MosIoSchematic, Output, Signal};
use substrate::layout::element::Shape;
use substrate::layout::{ExportsLayoutData, Layout};
use substrate::pdk::layers::HasPin;
use substrate::pdk::Pdk;
use substrate::schematic::schema::Schema;
use substrate::schematic::{CellBuilder, ExportsNestedData, Schematic};

pub mod tb;
pub mod tech;

#[derive(Debug, Default, Clone, Io)]
pub struct ClockedDiffComparatorIo {
    pub input: Input<DiffPair>,
    pub output: Output<DiffPair>,
    pub clock: Input<Signal>,
    pub vdd: InOut<Signal>,
    pub vss: InOut<Signal>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct StrongArmParams {
    pub half_tail_w: i64,
    pub input_pair_w: i64,
    pub inv_nmos_w: i64,
    pub inv_pmos_w: i64,
    pub precharge_w: i64,
}

/// The IO of a tap.
#[derive(Default, Debug, Clone, Copy, Io)]
pub struct TapIo {
    /// The tap contact.
    pub x: InOut<Signal>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum TileKind {
    N,
    P,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct MosTileParams {
    pub kind: TileKind,
    pub w: i64,
}

impl MosTileParams {
    pub fn new(kind: TileKind, w: i64) -> Self {
        Self { kind, w }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct TapTileParams {
    pub kind: TileKind,
    /// Number of MOS devices this tap must span.
    pub mos: i64,
    /// Number of dummy devices this tap must span.
    pub dummy: i64,
}

impl TapTileParams {
    pub fn new(kind: TileKind, mos: i64, dummy: i64) -> Self {
        Self { kind, mos, dummy }
    }
}

pub trait HasStrongArmImpl<PDK: Pdk + Schema> {
    type MosTile: Tile<PDK> + Block<Io = MosIo> + Clone;
    type DummyTile: Tile<PDK> + Block<Io = MosIo> + Clone;
    type TapTile: Tile<PDK> + Block<Io = TapIo> + Clone;
    type PortLayer: HasPin;
    type ViaMaker: ViaMaker<PDK>;

    fn mos(params: MosTileParams) -> Self::MosTile;
    fn dummy(params: MosTileParams) -> Self::DummyTile;
    fn tap(params: TapTileParams) -> Self::TapTile;
    fn via_maker() -> Self::ViaMaker;
    fn port_layer(layers: &<PDK as Pdk>::Layers) -> Self::PortLayer;
}

#[derive_where::derive_where(Copy, Clone, Debug, Hash, PartialEq, Eq)]
#[derive(Serialize, Deserialize)]
pub struct StrongArmHalf<T>(
    StrongArmParams,
    #[serde(bound(deserialize = ""))] PhantomData<fn() -> T>,
);

impl<T> StrongArmHalf<T> {
    pub fn new(params: StrongArmParams) -> Self {
        Self(params, PhantomData)
    }
}

impl<T: Any> Block for StrongArmHalf<T> {
    type Io = ClockedDiffComparatorIo;

    fn id() -> ArcStr {
        substrate::arcstr::literal!("strongarm")
    }

    fn name(&self) -> ArcStr {
        substrate::arcstr::literal!("strongarm")
    }

    fn io(&self) -> Self::Io {
        Default::default()
    }
}

impl<T: Any> ExportsNestedData for StrongArmHalf<T> {
    type NestedData = ();
}

impl<T: Any> ExportsLayoutData for StrongArmHalf<T> {
    type LayoutData = ();
}

impl<PDK: Pdk + Schema + Sized, T: HasStrongArmImpl<PDK> + Any> Tile<PDK> for StrongArmHalf<T> {
    fn tile<'a>(
        &self,
        io: IoBuilder<'a, Self>,
        cell: &mut TileBuilder<'a, PDK>,
    ) -> substrate::error::Result<(
        <Self as ExportsNestedData>::NestedData,
        <Self as ExportsLayoutData>::LayoutData,
    )> {
        let half_tail_params = MosTileParams::new(TileKind::N, self.0.half_tail_w);
        let input_pair_params = MosTileParams::new(TileKind::N, self.0.input_pair_w);
        let inv_nmos_params = MosTileParams::new(TileKind::N, self.0.inv_nmos_w);
        let inv_pmos_params = MosTileParams::new(TileKind::P, self.0.inv_pmos_w);
        let precharge_params = MosTileParams::new(TileKind::P, self.0.precharge_w);

        let tail = cell.signal("tail", Signal);
        let intn = cell.signal("intn", Signal);
        let intp = cell.signal("intp", Signal);

        let mut tail_dummy = cell.generate_connected(
            T::dummy(half_tail_params),
            MosIoSchematic {
                d: io.schematic.vss,
                g: io.schematic.vss,
                s: io.schematic.vss,
                b: io.schematic.vss,
            },
        );
        let mut tail_pair = (0..2)
            .map(|_| {
                cell.generate_connected(
                    T::mos(half_tail_params),
                    MosIoSchematic {
                        d: tail,
                        g: io.schematic.clock,
                        s: io.schematic.vss,
                        b: io.schematic.vss,
                    },
                )
            })
            .collect::<Vec<_>>();

        let mut ptap = cell.generate(T::tap(TapTileParams::new(TileKind::P, 2, 1)));
        let ntap = cell.generate(T::tap(TapTileParams::new(TileKind::N, 2, 1)));
        cell.connect(ptap.io().x, io.schematic.vss);
        cell.connect(ntap.io().x, io.schematic.vdd);

        let mut input_pair = (0..2)
            .map(|i| {
                cell.generate_connected(
                    T::mos(input_pair_params),
                    MosIoSchematic {
                        d: if i == 0 { intn } else { intp },
                        g: if i == 0 {
                            io.schematic.input.p
                        } else {
                            io.schematic.input.n
                        },
                        s: tail,
                        b: io.schematic.vss,
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut input_dummy = cell.generate_connected(
            T::dummy(input_pair_params),
            MosIoSchematic {
                d: io.schematic.vss,
                g: io.schematic.vss,
                s: io.schematic.vss,
                b: io.schematic.vss,
            },
        );
        let mut inv_nmos_pair = (0..2)
            .map(|i| {
                cell.generate_connected(
                    T::mos(inv_nmos_params),
                    if i == 0 {
                        MosIoSchematic {
                            d: io.schematic.output.n,
                            g: io.schematic.output.p,
                            s: intn,
                            b: io.schematic.vss,
                        }
                    } else {
                        MosIoSchematic {
                            d: io.schematic.output.p,
                            g: io.schematic.output.n,
                            s: intp,
                            b: io.schematic.vss,
                        }
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut inv_nmos_dummy = cell.generate_connected(
            T::dummy(inv_nmos_params),
            MosIoSchematic {
                d: io.schematic.vss,
                g: io.schematic.vss,
                s: io.schematic.vss,
                b: io.schematic.vss,
            },
        );
        let mut inv_pmos_pair = (0..2)
            .map(|i| {
                cell.generate_connected(
                    T::mos(inv_pmos_params),
                    MosIoSchematic {
                        d: if i == 0 {
                            io.schematic.output.n
                        } else {
                            io.schematic.output.p
                        },
                        g: if i == 0 {
                            io.schematic.output.p
                        } else {
                            io.schematic.output.n
                        },
                        s: io.schematic.vdd,
                        b: io.schematic.vdd,
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut inv_pmos_dummy = cell.generate_connected(
            T::dummy(inv_pmos_params),
            MosIoSchematic {
                d: io.schematic.vdd,
                g: io.schematic.vdd,
                s: io.schematic.vdd,
                b: io.schematic.vdd,
            },
        );
        let mut precharge_pair_a = (0..2)
            .map(|i| {
                cell.generate_connected(
                    T::mos(precharge_params),
                    MosIoSchematic {
                        d: if i == 0 {
                            io.schematic.output.n
                        } else {
                            io.schematic.output.p
                        },
                        g: io.schematic.clock,
                        s: io.schematic.vdd,
                        b: io.schematic.vdd,
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut precharge_pair_a_dummy = cell.generate_connected(
            T::dummy(precharge_params),
            MosIoSchematic {
                d: io.schematic.vdd,
                g: io.schematic.vdd,
                s: io.schematic.vdd,
                b: io.schematic.vdd,
            },
        );
        let mut precharge_pair_b = (0..2)
            .map(|i| {
                cell.generate_connected(
                    T::mos(precharge_params),
                    MosIoSchematic {
                        d: if i == 0 { intn } else { intp },
                        g: io.schematic.clock,
                        s: io.schematic.vdd,
                        b: io.schematic.vdd,
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut precharge_pair_b_dummy = cell.generate_connected(
            T::dummy(precharge_params),
            MosIoSchematic {
                d: io.schematic.vdd,
                g: io.schematic.vdd,
                s: io.schematic.vdd,
                b: io.schematic.vdd,
            },
        );

        let mut prev = ntap.lcm_bounds();

        for (dummy, mos_pair) in [
            (&mut precharge_pair_a_dummy, &mut precharge_pair_a),
            (&mut precharge_pair_b_dummy, &mut precharge_pair_b),
            (&mut inv_pmos_dummy, &mut inv_pmos_pair),
            (&mut inv_nmos_dummy, &mut inv_nmos_pair),
            (&mut input_dummy, &mut input_pair),
            (&mut tail_dummy, &mut tail_pair),
        ] {
            dummy.align_rect_mut(prev, AlignMode::Left, 0);
            dummy.align_rect_mut(prev, AlignMode::Beneath, 0);
            prev = dummy.lcm_bounds();
            mos_pair[0].align_rect_mut(prev, AlignMode::Bottom, 0);
            mos_pair[0].align_rect_mut(prev, AlignMode::ToTheRight, 0);
            let left_rect = mos_pair[0].lcm_bounds();
            mos_pair[1].align_rect_mut(left_rect, AlignMode::Bottom, 0);
            mos_pair[1].align_rect_mut(left_rect, AlignMode::ToTheRight, 0);
        }

        ptap.align_rect_mut(prev, AlignMode::Left, 0);
        ptap.align_rect_mut(prev, AlignMode::Beneath, 0);

        let strongarm_lcm_hspan = ptap.lcm_bounds().hspan();

        let ptap = cell.draw(ptap)?;
        let ntap = cell.draw(ntap)?;
        let tail_pair = tail_pair
            .into_iter()
            .map(|inst| cell.draw(inst))
            .collect::<Result<Vec<_>>>()?;
        let _tail_dummy = cell.draw(tail_dummy)?;
        let input_pair = input_pair
            .into_iter()
            .map(|inst| cell.draw(inst))
            .collect::<Result<Vec<_>>>()?;
        let _input_dummy = cell.draw(input_dummy)?;
        let _inv_nmos_pair = inv_nmos_pair
            .into_iter()
            .map(|inst| cell.draw(inst))
            .collect::<Result<Vec<_>>>()?;
        let _inv_nmos_dummy = cell.draw(inv_nmos_dummy)?;
        let inv_pmos_pair = inv_pmos_pair
            .into_iter()
            .map(|inst| cell.draw(inst))
            .collect::<Result<Vec<_>>>()?;
        let _inv_pmos_dummy = cell.draw(inv_pmos_dummy)?;
        let _precharge_pair_a = precharge_pair_a
            .into_iter()
            .map(|inst| cell.draw(inst))
            .collect::<Result<Vec<_>>>()?;
        let _precharge_pair_a_dummy = cell.draw(precharge_pair_a_dummy)?;
        let _precharge_pair_b = precharge_pair_b
            .into_iter()
            .map(|inst| cell.draw(inst))
            .collect::<Result<Vec<_>>>()?;
        let _precharge_pair_b_dummy = cell.draw(precharge_pair_b_dummy)?;

        cell.set_top_layer(2);
        cell.set_router(GreedyRouter);
        cell.set_via_maker(T::via_maker());

        io.layout.vdd.set_primary(ntap.layout.io().x.primary);
        io.layout.vss.set_primary(ptap.layout.io().x.primary);

        let m1slice = cell.layer_stack.slice(0..2);

        let mut lcm_tracks = Vec::new();
        lcm_tracks.push(
            m1slice
                .shrink_to_lcm_units(tail_pair[0].layout.io().g.primary.bbox().unwrap())
                .unwrap()
                .bot(),
        );
        for io in [input_pair[0].layout.io(), inv_pmos_pair[0].layout.io()] {
            let bot_track = m1slice
                .expand_to_lcm_units(io.g.primary.bbox().unwrap())
                .bot();
            lcm_tracks.push(bot_track);
            lcm_tracks.push(bot_track + 1);
        }

        for (i, port) in [
            io.schematic.clock,
            io.schematic.input.p,
            io.schematic.input.n,
            io.schematic.output.p,
            io.schematic.output.n,
        ]
        .into_iter()
        .enumerate()
        {
            cell.assign_grid_points(
                port,
                1,
                Rect::from_spans(
                    strongarm_lcm_hspan,
                    Span::new(lcm_tracks[i], lcm_tracks[i] + 1),
                ),
            );
        }

        let m1slice = cell.layer_stack.slice(0..2);

        let io_rects = lcm_tracks
            .into_iter()
            .map(|track| {
                m1slice
                    .lcm_to_physical_rect(Rect::from_spans(
                        strongarm_lcm_hspan,
                        Span::from_point(track),
                    ))
                    .expand_dir(Dir::Vert, 200)
            })
            .collect::<Vec<_>>();

        for (i, port) in [
            &mut io.layout.clock,
            &mut io.layout.input.p,
            &mut io.layout.input.n,
            &mut io.layout.output.p,
            &mut io.layout.output.n,
        ]
        .into_iter()
        .enumerate()
        {
            cell.layout
                .draw(Shape::new(m1slice.layer(1).id, io_rects[i]))?;
            port.set_primary(IoShape::with_layers(
                T::port_layer(&cell.ctx().layers),
                io_rects[i],
            ));
        }

        Ok(((), ()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sky130_ctx;
    use crate::strongarm::tb::{ComparatorDecision, StrongArmTranTb};
    use crate::strongarm::tech::sky130::Sky130;
    use atoll::TileWrapper;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sky130pdk::corner::Sky130Corner;
    use sky130pdk::Sky130CommercialSchema;
    use spice::netlist::NetlistOptions;
    use spice::Spice;
    use std::path::PathBuf;
    use substrate::pdk::corner::Pvt;
    use substrate::schematic::netlist::ConvertibleNetlister;

    #[test]
    fn strongarm_sim() {
        let work_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/build/strongarm_sim");
        let dut = TileWrapper::new(StrongArmHalf::<Sky130>::new(StrongArmParams {
            half_tail_w: 1_250,
            input_pair_w: 4_000,
            inv_nmos_w: 2_000,
            inv_pmos_w: 1_000,
            precharge_w: 1_000,
        }));
        let pvt = Pvt {
            corner: Sky130Corner::Tt,
            voltage: dec!(1.8),
            temp: dec!(25.0),
        };
        let ctx = sky130_ctx();

        for i in 3..=10 {
            for j in [
                dec!(-1.8),
                dec!(-0.5),
                dec!(-0.1),
                dec!(-0.05),
                dec!(0.05),
                dec!(0.1),
                dec!(0.5),
                dec!(1.8),
            ] {
                let vinn = dec!(0.18) * Decimal::from(i);
                let vinp = vinn + j;

                if vinp < dec!(0.5) || vinp > dec!(1.8) {
                    continue;
                }

                let tb = StrongArmTranTb {
                    dut: dut.clone(),
                    vinp,
                    vinn,
                    pvt,
                };
                let decision = ctx
                    .simulate(tb, work_dir)
                    .expect("failed to run simulation")
                    .expect("comparator output did not rail");
                assert_eq!(
                    decision,
                    if j > dec!(0) {
                        ComparatorDecision::Pos
                    } else {
                        ComparatorDecision::Neg
                    },
                    "comparator produced incorrect decision"
                );
            }
        }
    }

    #[test]
    fn strongarm_lvs() {
        let work_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/build/strongarm_lvs"));
        let gds_path = work_dir.join("layout.gds");
        let netlist_path = work_dir.join("netlist.sp");
        let ctx = sky130_ctx();

        let block = TileWrapper::new(StrongArmHalf::<Sky130>::new(StrongArmParams {
            half_tail_w: 1_250,
            input_pair_w: 4_000,
            inv_nmos_w: 2_000,
            inv_pmos_w: 1_000,
            precharge_w: 1_000,
        }));

        let scir = ctx
            .export_scir(block.clone())
            .unwrap()
            .scir
            .convert_schema::<Sky130CommercialSchema>()
            .unwrap()
            .convert_schema::<Spice>()
            .unwrap()
            .build()
            .unwrap();
        Spice
            .write_scir_netlist_to_file(&scir, netlist_path, NetlistOptions::default())
            .expect("failed to write netlist");

        ctx.write_layout(block, gds_path)
            .expect("failed to write layout");
    }
}
