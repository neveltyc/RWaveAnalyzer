#!/usr/bin/env python3
"""
Deterministic synthetic FST generator for rwave's cross-version perf baseline.

Models a multi-core SoC waveform with **realistic activity density**:
many declared signals, but most change rarely (regfile entries, CSRs).
A handful of always-on signals (clocks, pipeline stages) carry the bulk
of the events.

Three activity tiers (per main-clock rising edge):
    Tier 1 — clocks + pipeline + a few control sigs: ~every cycle
    Tier 2 — buses, ALU/LSU operands:                ~10% of cycles
    Tier 3 — regfile entries, CSRs:                  ~0.1% of cycles

Output is fully deterministic: fixed LFSR seed, no PRNG, no wall-clock
dependency. Two runs of the same gen.py version produce identical FST
bytes — version-to-version rwave benchmark comparisons are meaningful.

Backend: pylibfst (cffi wrapper around GTKWave libfst).

Sizes (approximate post-LZ4 FST):
    --size small    ~5 MB     ~3 s     (local iteration)
    --size medium   ~30 MB    ~15 s    (CI default, on tag)
    --size huge     ~100 MB   ~2 min   (opt-in stress, local only)
"""
from __future__ import annotations

import argparse
import struct
import sys
import time
from pathlib import Path

import pylibfst as pf


# ---------- size knobs --------------------------------------------------------
# (clusters × cores) × (signals per core ~71) + ~250 upper-level signals.
#   4 × 16 → ~4800 signals.
#   2 ×  8 → ~1400 signals.

SIZES = {
    "small":  {"clusters": 2, "cores":  8, "sim_ticks":   200_000, "label":  "~5 MB"},
    "medium": {"clusters": 4, "cores": 16, "sim_ticks": 1_500_000, "label": "~30 MB"},
    "huge":   {"clusters": 4, "cores": 16, "sim_ticks": 5_000_000, "label": "~100 MB"},
}


# ---------- deterministic LFSR (32-bit) ---------------------------------------

class LFSR:
    __slots__ = ("state",)

    def __init__(self, seed: int) -> None:
        self.state = seed & 0xFFFFFFFF or 0xDEADBEEF

    def next32(self) -> int:
        s = self.state
        s = ((s << 1) & 0xFFFFFFFF) ^ ((0 - (s >> 31)) & 0xA3000000)
        self.state = s
        return s

    def chance(self, num: int, denom: int) -> bool:
        return (self.next32() % denom) < num


# ---------- bit-string helpers ------------------------------------------------

def _fmt(width: int):
    spec = f"0{width}b"
    return lambda v: format(v & ((1 << width) - 1), spec).encode("ascii")

bits1, bits3, bits4, bits8 = _fmt(1), _fmt(3), _fmt(4), _fmt(8)
bits16, bits32, bits64, bits128, bits256 = _fmt(16), _fmt(32), _fmt(64), _fmt(128), _fmt(256)


# ---------- the hierarchy ----------------------------------------------------

def build(ctx, n_clusters: int, n_cores: int):
    """Declare scopes and signals; return a dict of handles."""
    H: dict = {}

    SCOPE = pf.lib.FST_ST_VCD_MODULE
    WIRE  = pf.lib.FST_VT_VCD_WIRE
    REG   = pf.lib.FST_VT_VCD_REG
    REAL  = pf.lib.FST_VT_VCD_REAL
    IMPL  = pf.lib.FST_VD_IMPLICIT

    def scope(name: str):
        pf.lib.fstWriterSetScope(ctx, SCOPE, name.encode(), b"")

    def upscope():
        pf.lib.fstWriterSetUpscope(ctx)

    def var(t, w: int, name: str):
        return pf.lib.fstWriterCreateVar(ctx, t, IMPL, w, name.encode(), 0)

    scope("tb")
    H["clk_main"]   = var(WIRE, 1, "clk_main")
    H["clk_periph"] = var(WIRE, 1, "clk_periph")
    H["rst_n"]      = var(WIRE, 1, "rst_n")
    H["stim_seed"]  = var(REG, 32, "stim_seed")
    H["chip_active"] = var(WIRE, 1, "chip_active")

    scope("soc")
    H["soc_busy"]   = var(WIRE,  1, "soc_busy")
    H["soc_status"] = var(REG,  32, "soc_status")
    # NoC: a few channels with addr/data/valid
    H["noc_channels"] = []
    for ch in range(8):
        scope(f"noc_{ch}")
        c = {
            "addr":  var(WIRE,  32, "addr"),
            "data":  var(WIRE, 128, "data"),
            "valid": var(WIRE,   1, "valid"),
            "ready": var(WIRE,   1, "ready"),
            "burst": var(REG,    4, "burst"),
        }
        upscope()
        H["noc_channels"].append(c)

    H["clusters"] = []
    for cl_i in range(n_clusters):
        scope(f"cluster_{cl_i}")
        cl = {
            "busy":      var(WIRE,  1, "busy"),
            "egress":    var(WIRE, 64, "egress"),
            "credit":    var(REG,   8, "credit"),
            "l2_state":  var(REG,   4, "l2_state"),
            "l2_addr":   var(WIRE, 32, "l2_addr"),
            # Wide entropy-rich data buses — change every cycle with random
            # values to ensure FST size grows with simulation work (otherwise
            # LZ4 squashes the regular patterns of pipeline state to <1 MB).
            "mem_data":  [var(WIRE, 256, f"mem_data_{k}") for k in range(8)],
            "cores":     [],
        }
        for co_i in range(n_cores):
            scope(f"core_{co_i}")
            co = {
                # Tier 1: pipeline / control (active every cycle)
                "pc":         var(REG, 32, "pc"),
                "ir":         var(REG, 32, "ir"),
                "stage":      var(REG,  3, "stage"),
                "flush":      var(WIRE, 1, "flush"),
                "stall":      var(WIRE, 1, "stall"),
                "retire":     var(WIRE, 1, "retire"),
                "wb_we":      var(WIRE, 1, "wb_we"),
                "wb_rd":      var(REG,  5, "wb_rd"),
                # Tier 2: ALU / LSU (active on demand)
                "alu_op":     var(REG,  4, "alu_op"),
                "alu_a":      var(REG, 64, "alu_a"),
                "alu_b":      var(REG, 64, "alu_b"),
                "alu_r":      var(REG, 64, "alu_r"),
                "lsu_addr":   var(REG, 32, "lsu_addr"),
                "lsu_wdata":  var(REG, 64, "lsu_wdata"),
                "lsu_rdata":  var(REG, 64, "lsu_rdata"),
                "lsu_we":     var(WIRE, 1, "lsu_we"),
                "lsu_re":     var(WIRE, 1, "lsu_re"),
                # Tier 3: rarely-touched state
                "exception":  var(WIRE, 1, "exception"),
                "int_pend":   var(WIRE, 1, "int_pend"),
                "regs":       [],
                "csrs":       [],
            }
            scope("regfile")
            co["regs"] = [var(REG, 64, f"x{r}") for r in range(32)]
            upscope()
            scope("csrs")
            co["csrs"] = [var(REG, 64, f"csr_{c}") for c in range(16)]
            upscope()
            cl["cores"].append(co)
            upscope()  # core_co_i
        H["clusters"].append(cl)
        upscope()  # cluster_cl_i

    scope("peripheral")
    H["uart_tx"]   = var(WIRE, 1,  "uart_tx")
    H["uart_data"] = var(REG,  8,  "uart_data")
    H["uart_baud"] = var(REAL, 64, "uart_baud")
    H["timer_cnt"] = var(REG, 32,  "timer_cnt")
    H["timer_irq"] = var(WIRE, 1,  "timer_irq")
    H["gpio"]      = var(WIRE, 16, "gpio")
    H["gpio_oe"]   = var(REG, 16,  "gpio_oe")
    upscope()  # peripheral

    upscope()  # soc
    upscope()  # tb

    return H


# ---------- the simulation loop ----------------------------------------------

def simulate(ctx, H: dict, sim_ticks: int, n_clusters: int, n_cores: int) -> int:
    """Drive a tiered-activity simulation. Returns total value-change count."""
    lfsr = LFSR(0xDEADBEEF)
    n = 0

    emit_tc = pf.lib.fstWriterEmitTimeChange
    emit_vc = pf.lib.fstWriterEmitValueChange
    chance = lfsr.chance
    next32 = lfsr.next32

    clusters = H["clusters"]
    noc = H["noc_channels"]

    # ---- t=0 baseline ----------------------------------------------------
    emit_tc(ctx, 0)
    emit_vc(ctx, H["clk_main"], b"0"); n += 1
    emit_vc(ctx, H["clk_periph"], b"0"); n += 1
    emit_vc(ctx, H["rst_n"], b"0"); n += 1
    emit_vc(ctx, H["stim_seed"], bits32(0)); n += 1
    emit_vc(ctx, H["chip_active"], b"0"); n += 1
    emit_vc(ctx, H["soc_busy"], b"0"); n += 1
    emit_vc(ctx, H["soc_status"], bits32(0)); n += 1
    for c in noc:
        emit_vc(ctx, c["addr"], bits32(0));    n += 1
        emit_vc(ctx, c["data"], bits128(0));   n += 1
        emit_vc(ctx, c["valid"], b"0");        n += 1
        emit_vc(ctx, c["ready"], b"1");        n += 1
        emit_vc(ctx, c["burst"], bits4(0));    n += 1
    for cl in clusters:
        emit_vc(ctx, cl["busy"], b"0");           n += 1
        emit_vc(ctx, cl["egress"], bits64(0));    n += 1
        emit_vc(ctx, cl["credit"], bits8(0xFF));  n += 1
        emit_vc(ctx, cl["l2_state"], bits4(0));   n += 1
        emit_vc(ctx, cl["l2_addr"], bits32(0));   n += 1
        for md in cl["mem_data"]:
            emit_vc(ctx, md, bits256(0)); n += 1
        for co in cl["cores"]:
            emit_vc(ctx, co["pc"], bits32(0)); n += 1
            emit_vc(ctx, co["ir"], bits32(0)); n += 1
            emit_vc(ctx, co["stage"], bits3(0)); n += 1
            emit_vc(ctx, co["flush"], b"0"); n += 1
            emit_vc(ctx, co["stall"], b"0"); n += 1
            emit_vc(ctx, co["retire"], b"0"); n += 1
            emit_vc(ctx, co["wb_we"], b"0"); n += 1
            emit_vc(ctx, co["wb_rd"], b"00000"); n += 1
            emit_vc(ctx, co["alu_op"], bits4(0)); n += 1
            emit_vc(ctx, co["alu_a"], bits64(0)); n += 1
            emit_vc(ctx, co["alu_b"], bits64(0)); n += 1
            emit_vc(ctx, co["alu_r"], bits64(0)); n += 1
            emit_vc(ctx, co["lsu_addr"], bits32(0)); n += 1
            emit_vc(ctx, co["lsu_wdata"], bits64(0)); n += 1
            emit_vc(ctx, co["lsu_rdata"], bits64(0)); n += 1
            emit_vc(ctx, co["lsu_we"], b"0"); n += 1
            emit_vc(ctx, co["lsu_re"], b"0"); n += 1
            emit_vc(ctx, co["exception"], b"0"); n += 1
            emit_vc(ctx, co["int_pend"], b"0"); n += 1
            for r in co["regs"]:
                emit_vc(ctx, r, bits64(0)); n += 1
            for c in co["csrs"]:
                emit_vc(ctx, c, bits64(0)); n += 1
    emit_vc(ctx, H["uart_tx"], b"1");        n += 1
    emit_vc(ctx, H["uart_data"], bits8(0));  n += 1
    emit_vc(ctx, H["uart_baud"], struct.pack("<d", 115200.0)); n += 1
    emit_vc(ctx, H["timer_cnt"], bits32(0)); n += 1
    emit_vc(ctx, H["timer_irq"], b"0");      n += 1
    emit_vc(ctx, H["gpio"], b"zzzzzzzzzzzzzzzz"); n += 1
    emit_vc(ctx, H["gpio_oe"], bits16(0));   n += 1

    emit_tc(ctx, 100)
    emit_vc(ctx, H["rst_n"], b"1"); n += 1

    # ---- main loop -------------------------------------------------------
    pc = [[0] * n_cores for _ in range(n_clusters)]
    stage = [[0] * n_cores for _ in range(n_clusters)]
    stall_until = [[0] * n_cores for _ in range(n_clusters)]
    timer_cnt = 0
    gpio_oe = 0
    uart_byte = 0
    stim = 0x12345678
    MASK64 = (1 << 64) - 1

    t = 100
    while t < sim_ticks:
        # 100 MHz main clock — every 5 ticks
        if t % 5 == 0:
            emit_tc(ctx, t)
            phase = (t // 5) & 1
            emit_vc(ctx, H["clk_main"], b"1" if phase else b"0"); n += 1

            if phase:  # rising edge
                # ---- Tier-1 background: top-level stim/status -------------
                stim = (stim ^ (stim << 1) ^ (stim >> 3)) & 0xFFFFFFFF
                emit_vc(ctx, H["stim_seed"], bits32(stim)); n += 1

                # ---- Tier-2: NoC and soc-level (~10% events per channel) --
                for c in noc:
                    if chance(1, 10):
                        emit_vc(ctx, c["addr"],  bits32(next32())); n += 1
                        emit_vc(ctx, c["data"],  bits128((next32() << 96) | (next32() << 64)
                                                         | (next32() << 32) | next32())); n += 1
                        emit_vc(ctx, c["valid"], b"1"); n += 1
                    elif chance(1, 8):
                        emit_vc(ctx, c["valid"], b"0"); n += 1
                if chance(1, 100):  # Tier-3
                    emit_vc(ctx, H["soc_status"], bits32(stim)); n += 1

                # ---- per-cluster (Tier 1/2/3 mixed) ----------------------
                any_busy = False
                for ci, cl in enumerate(clusters):
                    cluster_busy = False
                    # Tier 2: egress / credit
                    if chance(1, 10):
                        emit_vc(ctx, cl["egress"], bits64((stim ^ (ci * 0x1111_1111_1111_1111)) & MASK64)); n += 1
                        cluster_busy = True
                    if chance(1, 50):
                        emit_vc(ctx, cl["credit"], bits8((stim >> ci) & 0xFF)); n += 1
                    # Tier 3: L2 cache state changes occasionally
                    if chance(1, 200):
                        emit_vc(ctx, cl["l2_state"], bits4(next32() & 0xF)); n += 1
                        emit_vc(ctx, cl["l2_addr"],  bits32(next32())); n += 1
                    # 256-bit memory data channels — entropy-rich, ~30% per channel
                    for md in cl["mem_data"]:
                        if chance(3, 10):
                            big = ((next32() << 224) | (next32() << 192)
                                 | (next32() << 160) | (next32() << 128)
                                 | (next32() <<  96) | (next32() <<  64)
                                 | (next32() <<  32) |  next32())
                            emit_vc(ctx, md, bits256(big)); n += 1

                    # ---- per-core ------------------------------------------
                    for co_i, co in enumerate(cl["cores"]):
                        stalled = stall_until[ci][co_i] > t

                        # Tier 1: pipeline always advances unless stalled
                        if not stalled:
                            pc[ci][co_i] = (pc[ci][co_i] + 4) & 0xFFFFFFFF
                            emit_vc(ctx, co["pc"], bits32(pc[ci][co_i])); n += 1
                            emit_vc(ctx, co["ir"], bits32(next32())); n += 1
                        stage[ci][co_i] = (stage[ci][co_i] + 1) % 6
                        emit_vc(ctx, co["stage"], bits3(stage[ci][co_i])); n += 1

                        # Occasional flush/stall (Tier 2-ish)
                        if chance(1, 30):
                            emit_vc(ctx, co["stall"], b"1" if not stalled else b"0"); n += 1
                            stall_until[ci][co_i] = t + (next32() % 20)
                        if chance(1, 100):
                            emit_vc(ctx, co["flush"], b"1"); n += 1

                        # retire fires more often than not
                        if not stalled and chance(7, 10):
                            emit_vc(ctx, co["retire"], b"1"); n += 1
                            cluster_busy = True
                        elif chance(2, 10):
                            emit_vc(ctx, co["retire"], b"0"); n += 1

                        # Tier 2: ALU activity (when not stalled, ~50%)
                        if not stalled and chance(5, 10):
                            a = next32(); b = next32()
                            emit_vc(ctx, co["alu_op"], bits4(a & 0xF)); n += 1
                            emit_vc(ctx, co["alu_a"], bits64(a | (a << 32))); n += 1
                            emit_vc(ctx, co["alu_b"], bits64(b | (b << 32))); n += 1
                            emit_vc(ctx, co["alu_r"], bits64((a + b) & MASK64)); n += 1
                        # Tier 2: LSU activity (~20%)
                        if not stalled and chance(2, 10):
                            emit_vc(ctx, co["lsu_addr"], bits32(next32())); n += 1
                            if chance(1, 2):
                                emit_vc(ctx, co["lsu_wdata"], bits64((next32() << 32) | next32())); n += 1
                                emit_vc(ctx, co["lsu_we"], b"1"); n += 1
                            else:
                                emit_vc(ctx, co["lsu_rdata"], bits64((next32() << 32) | next32())); n += 1
                                emit_vc(ctx, co["lsu_re"], b"1"); n += 1
                        # Tier 1 control: writeback enable + dest reg
                        if not stalled and chance(6, 10):
                            rd = next32() & 0x1F
                            emit_vc(ctx, co["wb_we"], b"1"); n += 1
                            emit_vc(ctx, co["wb_rd"], format(rd, "05b").encode()); n += 1
                            # Tier 3: actual regfile write follows
                            if rd != 0 and chance(1, 1):  # 100% when wb_we
                                emit_vc(ctx, co["regs"][rd], bits64((next32() << 32) | next32())); n += 1
                        # Tier 3: rare exception / interrupt / CSR write
                        if chance(1, 1000):
                            emit_vc(ctx, co["exception"], b"1"); n += 1
                        if chance(1, 500):
                            emit_vc(ctx, co["int_pend"], b"1"); n += 1
                        if chance(1, 300):
                            csr_idx = next32() & 0xF
                            emit_vc(ctx, co["csrs"][csr_idx], bits64((next32() << 32) | next32())); n += 1

                    if cluster_busy:
                        emit_vc(ctx, cl["busy"], b"1"); n += 1
                        any_busy = True
                    elif chance(1, 5):
                        emit_vc(ctx, cl["busy"], b"0"); n += 1

                emit_vc(ctx, H["soc_busy"],    b"1" if any_busy else b"0"); n += 1
                emit_vc(ctx, H["chip_active"], b"1" if any_busy else b"0"); n += 1

        # 42 MHz peripheral clock — every 12 ticks
        if t % 12 == 0:
            emit_tc(ctx, t)
            phase = (t // 12) & 1
            emit_vc(ctx, H["clk_periph"], b"1" if phase else b"0"); n += 1
            if phase:
                timer_cnt = (timer_cnt + 1) & 0xFFFFFFFF
                emit_vc(ctx, H["timer_cnt"], bits32(timer_cnt)); n += 1
                if timer_cnt % 1000 == 0:
                    emit_vc(ctx, H["timer_irq"], b"1"); n += 1
                elif (timer_cnt - 1) % 1000 == 0:
                    emit_vc(ctx, H["timer_irq"], b"0"); n += 1
                uart_byte = (uart_byte + 1) & 0xFF
                emit_vc(ctx, H["uart_data"], bits8(uart_byte)); n += 1
                emit_vc(ctx, H["uart_tx"], b"1" if (uart_byte & 1) else b"0"); n += 1
                if chance(1, 4):
                    gpio_oe = next32() & 0xFFFF
                    emit_vc(ctx, H["gpio_oe"], bits16(gpio_oe)); n += 1
                    out = bytearray(b"z" * 16)
                    for k in range(16):
                        if gpio_oe & (1 << (15 - k)):
                            out[k] = 49 if (stim >> k) & 1 else 48
                    emit_vc(ctx, H["gpio"], bytes(out)); n += 1

        t += 1

    return n


# ---------- entry --------------------------------------------------------------

def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--size", choices=list(SIZES.keys()), default="medium",
                   help="dataset preset (default: medium ~30 MB)")
    p.add_argument("--output", "-o", default="bench/build/stress.fst",
                   help="output FST path")
    args = p.parse_args()

    cfg = SIZES[args.size]
    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        out.unlink()

    print(f"=== bench/gen.py — size={args.size} ({cfg['label']}) ===")
    t0 = time.perf_counter()

    ctx = pf.lib.fstWriterCreate(str(out).encode(), 1)
    pf.lib.fstWriterSetTimescale(ctx, -9)
    pf.lib.fstWriterSetPackType(ctx, pf.lib.FST_WR_PT_LZ4)
    pf.lib.fstWriterSetVersion(ctx, b"rwave-bench gen.py v2")

    H = build(ctx, cfg["clusters"], cfg["cores"])

    # Approximate signal count: 5 tb + 2 soc + 8*5 noc + clusters*(5 + cores*(18 + 48)) + 7 periph
    n_sigs = 5 + 2 + 8*5 + cfg["clusters"]*(5 + cfg["cores"]*(18 + 32 + 16)) + 7
    print(f"  hierarchy: ~{n_sigs} signals, "
          f"{cfg['clusters']} clusters × {cfg['cores']} cores, "
          f"sim {cfg['sim_ticks']:,} ticks")

    t_emit_start = time.perf_counter()
    n_changes = simulate(ctx, H, cfg["sim_ticks"], cfg["clusters"], cfg["cores"])
    t_emit = time.perf_counter() - t_emit_start

    pf.lib.fstWriterClose(ctx)
    total = time.perf_counter() - t0
    size_mb = out.stat().st_size / 1_000_000
    print(f"  emit:  {t_emit:.1f}s, {n_changes:,} changes ({n_changes/t_emit/1e6:.2f}M/s)")
    print(f"  wrote: {out} ({size_mb:.1f} MB, total {total:.1f}s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
