// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)
//
// One design, one instance of every way SystemVerilog expresses connectivity.
//
// `dut.sv` is a small design that looks like RTL; this one is not meant to look
// like anything. It exists because the reader was wrong about interfaces,
// struct members, vector splits and generate blocks, and each of those was
// found by accident on somebody else's design. The construct list comes from
// slang's syntax node table rather than from memory: the families that can put
// two names on one net, or hide a statement somewhere a walk has to find it.
//
// Every construct sits in a scope named after it, so a disagreement names the
// construct rather than a line number. There are no expected answers written
// down — QuestaSim answers the same questions from the same file, and
// `diff.py` compares. What this file has to be is exhaustive, not clever.

`timescale 1ns/1ps

// ---------------------------------------------------------------- types

package cx_pkg;
  typedef struct packed {
    logic [7:0] addr;
    logic [3:0] len;
    logic       valid;
  } req_t;

  typedef struct packed {
    req_t       req;      // nested struct, packed
    logic [1:0] tag;
  } wrapped_t;

  // A packed union's members are all one width, which is what makes the two
  // spellings the same bits.
  typedef union packed {
    logic [12:0] raw;
    req_t        as_req;
  } view_t;

  typedef enum logic [1:0] {IDLE, BUSY, DONE} state_e;

  function automatic logic [7:0] twice(input logic [7:0] x);
    return {x[6:0], 1'b0};
  endfunction
endpackage

// ---------------------------------------------------------------- interface

interface cx_if (input logic clk);
  logic       vld;
  logic [7:0] data;
  logic       rdy;

  modport src (output vld, output data, input rdy, input clk);
  modport snk (input vld, input data, output rdy, input clk);
endinterface

// -------------------------------------------------------------- port forms

// ANSI ports, every direction and kind.
module cx_ansi (
    input  wire        i_wire,
    input  logic [7:0] i_vec,
    output logic       o_reg,
    output wire  [7:0] o_wire_vec,
    inout  wire        io_net
);
  assign o_wire_vec = i_vec ^ 8'h5a;              // ContinuousAssign, whole
  always_comb o_reg = i_wire & i_vec[0];          // always_comb
  assign io_net = i_wire ? 1'b0 : 1'bz;           // inout driven conditionally
endmodule

// Non-ANSI ports: the declaration is separate from the header.
module cx_nonansi (a, b, y);
  input  [3:0] a;
  input  [3:0] b;
  output [3:0] y;
  wire   [3:0] y;
  assign y = a | b;
endmodule

// A port that is a concatenation of two nets, and one declared with an
// explicit external name.
module cx_portconcat (.ext_pair({hi, lo}), .ext_one(inner));
  output hi, lo;
  input  inner;
  assign hi = inner;
  assign lo = ~inner;
endmodule

// ------------------------------------------------------- assignment forms

module cx_assigns (
    input  logic [15:0] src,
    input  logic [3:0]  idx,
    output logic [15:0] whole,
    output logic [15:0] parts,
    output logic [7:0]  packed_two,
    output logic        one_bit
);
  assign whole = src;                              // whole
  assign parts[7:0] = src[15:8];                   // part-select both sides
  assign parts[15:8] = src[7:0];
  assign one_bit = src[idx];                       // variable bit-select
  assign packed_two = {src[1:0], {3{src[2]}}, 3'b0}; // concat + replication

  logic [7:0] lo_half, hi_half;
  assign {hi_half, lo_half} = src;                 // concatenation as target

  logic [7:0] indexed;
  assign indexed = src[idx +: 8];                  // indexed part-select

  logic [7:0] fn_out;
  assign fn_out = cx_pkg::twice(src[7:0]);         // function call in an assign
endmodule

// ------------------------------------------------------- procedural forms

module cx_procedural (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [7:0] d,
    output logic [7:0] q_ff,
    output logic [7:0] q_latch,
    output logic [7:0] q_star
);
  always_ff @(posedge clk or negedge rst_n)        // async reset
    if (!rst_n) q_ff <= 8'h00;
    else if (en) q_ff <= d;                        // hold path reads q_ff

  always_latch
    if (en) q_latch = d;

  always @(*) q_star = d ^ q_ff;                   // implicit sensitivity

  logic [7:0] init_val;
  initial init_val = 8'hA5;                        // initial block

  // A task that writes a module signal, called from a process.
  logic [7:0] via_task;
  task automatic bump(input logic [7:0] x);
    via_task <= x + 8'd1;
  endtask
  always_ff @(posedge clk) bump(d);

  // A memory, written and read with a variable index.
  logic [7:0] mem [0:15];
  always_ff @(posedge clk) if (en) mem[d[3:0]] <= d;
  logic [7:0] mem_out;
  assign mem_out = mem[d[3:0]];
endmodule

// ------------------------------------------------------------ net types

module cx_nets (input logic a, input logic b, output logic wand_out);
  wand  w_and;                                     // two drivers, resolved
  assign w_and = a;
  assign w_and = b;
  assign wand_out = w_and;

  wor   w_or;
  assign w_or = a;
  assign w_or = b;

  tri   t_net;
  assign t_net = a ? b : 1'bz;

  // An implicit net: never declared, created by the connection below.
  cx_ansi u_implicit (
      .i_wire(a), .i_vec(8'h00), .o_reg(implicit_wire),
      .o_wire_vec(), .io_net()
  );
endmodule

// ---------------------------------------------------------- struct ports

module cx_struct (
    input  cx_pkg::wrapped_t in_w,                 // nested packed struct port
    input  cx_pkg::view_t    in_u,                 // packed union port
    output cx_pkg::req_t     out_r,
    output logic [7:0]       out_addr
);
  // Members read individually — the shapes hang off the member, not the whole.
  assign out_addr = in_w.req.addr;
  assign out_r.addr = in_w.req.addr ^ 8'hff;
  assign out_r.len = in_w.tag == 2'b11 ? 4'hf : in_w.req.len;
  assign out_r.valid = in_u.as_req.valid;
endmodule

// -------------------------------------------------- arrays and generates

module cx_lane #(parameter int LANE = 0) (
    input  logic       clk,
    input  logic [7:0] d,
    output logic [7:0] q
);
  always_ff @(posedge clk) q <= d + 8'(LANE);
endmodule

module cx_generate (
    input  logic       clk,
    input  logic [7:0] d,
    output logic [7:0] sum_named,
    output logic [7:0] sum_unnamed,
    output logic [7:0] from_if,
    output logic [7:0] from_case
);
  localparam int N = 4;
  logic [7:0] lane_q [N];

  // for-generate with a named block
  genvar gi;
  generate
    for (gi = 0; gi < N; gi++) begin : gen_lane
      cx_lane #(.LANE(gi)) u_lane (.clk(clk), .d(d), .q(lane_q[gi]));
    end
  endgenerate

  // for-generate with no label at all
  logic [7:0] anon_q [2];
  generate
    for (gi = 0; gi < 2; gi++) begin
      always_ff @(posedge clk) anon_q[gi] <= d ^ 8'(gi);
    end
  endgenerate

  // if-generate, both arms present in the source
  generate
    if (N > 2) begin : gen_wide
      assign from_if = lane_q[3];
    end else begin : gen_narrow
      assign from_if = lane_q[0];
    end
  endgenerate

  // case-generate
  generate
    case (N)
      4: begin : gen_case4
        assign from_case = lane_q[2];
      end
      default: begin : gen_case_other
        assign from_case = 8'h00;
      end
    endcase
  endgenerate

  // nested generate: a loop inside a loop, both named
  logic [7:0] grid [2][2];
  generate
    for (gi = 0; gi < 2; gi++) begin : gen_outer
      genvar gj;
      for (gj = 0; gj < 2; gj++) begin : gen_inner
        assign grid[gi][gj] = lane_q[gi] + lane_q[gj];
      end
    end
  endgenerate

  assign sum_named = lane_q[0] + lane_q[1] + lane_q[2] + lane_q[3];
  assign sum_unnamed = anon_q[0] + anon_q[1] + grid[1][1];
endmodule

// An array of instances, and gate primitives.
module cx_arrays (
    input  logic [3:0] a,
    input  logic [3:0] b,
    output logic [3:0] y,
    output logic       g_and,
    output logic       g_or,
    output logic       g_not,
    output logic       g_bufif
);
  cx_nonansi u_arr [3:0] (.a(a), .b(b), .y(y));    // array of instances

  and  u_and  (g_and, a[0], b[0]);                 // PrimitiveInstantiation
  or   u_or   (g_or, a[1], b[1]);
  not  u_not  (g_not, a[2]);
  bufif1 u_bufif (g_bufif, a[3], b[3]);
endmodule

// ---------------------------------------------------- interface consumers

module cx_if_src (cx_if.src bus, input logic [7:0] payload);
  assign bus.vld = |payload;                       // write through a modport
  assign bus.data = payload;
endmodule

module cx_if_snk (cx_if.snk bus, output logic [7:0] seen);
  always_ff @(posedge bus.clk) if (bus.vld) seen <= bus.data;
  assign bus.rdy = 1'b1;
endmodule

// A module taking the whole interface rather than a modport.
module cx_if_whole (cx_if bus, output logic saw_rdy);
  assign saw_rdy = bus.rdy & bus.vld;
endmodule

// An array of interfaces passed to an array of instances.
module cx_if_array (cx_if.snk bus [2], output logic [1:0] any);
  assign any[0] = bus[0].vld;
  assign any[1] = bus[1].vld;
endmodule

// ------------------------------------------------- hierarchical reference

module cx_hier_target (input logic clk, output logic [7:0] visible);
  logic [7:0] deep;
  always_ff @(posedge clk) deep <= deep + 8'd1;
  assign visible = deep;
  // Written from the top by hierarchical name: the only driver of this one
  // is a statement in a scope that cannot be reached from the net either.
  logic [7:0] poked;
endmodule

module cx_hier_reader (input logic clk, output logic [7:0] copied);
  // Reads a signal by hierarchical name from a sibling instance in the top.
  always_ff @(posedge clk) copied <= tb.u_hier_target.deep;
endmodule

// A module a `bind` attaches to the design from outside.
module cx_bound (input logic clk, input logic [7:0] watched, output logic seen_hi);
  always_ff @(posedge clk) seen_hi <= watched[7];
endmodule

// ------------------------------------------------- the second sweep of forms
//
// Everything above is what the four faults found so far were about. These are
// the rest of the families in the syntax table that can put a name on a net or
// hide a statement: the ones a design is less likely to contain, which is
// exactly why a fixture has to.

// A user-defined primitive: a table, not a statement, driving a net.
primitive cx_udp (out, a, b);
  output out;
  input  a, b;
  table
    // a b : out
       0 0 : 0;
       0 1 : 1;
       1 0 : 1;
       1 1 : 0;
  endtable
endprimitive

module cx_prim_user (input logic a, input logic b, output logic y);
  cx_udp u_udp (y, a, b);
endmodule

// A parameter overridden by `defparam` rather than by the instantiation.
module cx_param (input logic [7:0] d, output logic [7:0] q);
  parameter int SHIFT = 0;
  assign q = d << SHIFT;
endmodule

// Unpacked types, and a multi-dimensional packed array, as ports.
typedef struct {
  logic [7:0] a;
  logic [7:0] b;
} cx_unpacked_t;

module cx_shapes (
    input  cx_unpacked_t     up_in,      // unpacked struct port
    input  logic [3:0][7:0]  md_in,      // multi-dimensional packed array
    input  logic [7:0]       arr_in [2], // unpacked array port
    output logic [7:0]       up_sum,
    output logic [7:0]       md_pick,
    output logic [7:0]       arr_pick
);
  assign up_sum = up_in.a + up_in.b;
  assign md_pick = md_in[2];
  assign arr_pick = arr_in[1];
endmodule

// A named block inside a process, with a variable of its own: the variable
// gets no `signal_tbl` row anywhere, and only the top level records who
// touches it.
module cx_named_block (input logic clk, input logic [7:0] d, output logic [7:0] q);
  always_ff @(posedge clk) begin : accumulate
    logic [7:0] local_acc;
    local_acc = d + 8'd3;
    q <= local_acc;
  end
endmodule

// A `for` loop inside a process writing an array, and a streaming operator.
module cx_loops (input logic [31:0] d, output logic [7:0] bytes [4], output logic [31:0] rev);
  always_comb begin
    for (int i = 0; i < 4; i++) bytes[i] = d[i*8 +: 8];
  end
  assign rev = {<<8{d}};                            // streaming concatenation
endmodule

// Procedural continuous assignment, and force/release from a process.
module cx_forced (input logic clk, input logic sel, input logic [7:0] d, output logic [7:0] q);
  logic [7:0] held;
  always @(posedge clk) begin
    if (sel) force held = d;
    else     release held;
  end
  assign q = held;
endmodule

// An interface with a method, called across the port.
interface cx_meth_if;
  logic [7:0] store;
  function automatic logic [7:0] peek();
    return store;
  endfunction
  modport user (import peek, output store);
endinterface

module cx_meth_user (cx_meth_if.user bus, input logic [7:0] d, output logic [7:0] got);
  assign bus.store = d;
  assign got = bus.peek();
endmodule

// -------------------------------------------------- the third sweep of forms
//
// What the first two sweeps left: the ways a net gets a driver without an
// `assign` statement to point at, and the places a name can be reached from
// that are not the scope it sits in.

module cx_decl_assign (input logic a, input logic b, output logic y, output logic z);
  wire w = a & b;                                   // assignment in the declaration
  assign #2 y = w;                                  // a delay on a continuous assign
  wire (strong1, weak0) s = a;                      // drive strength
  assign z = s | w;
endmodule

// Bidirectional switches: the connection has no direction at all.
module cx_switch (inout wire x, inout wire y, input logic en);
  tranif1 u_t1 (x, y, en);
  tran    u_t0 (x, y);
endmodule

// The rest of the gate primitives a gate-level netlist is made of.
module cx_gates (input logic a, input logic b, input logic en,
                 output logic o_nand, output logic o_nor, output logic o_xnor,
                 output logic o_notif, output logic o_pull);
  nand   u_nand  (o_nand, a, b);
  nor    u_nor   (o_nor, a, b);
  xnor   u_xnor  (o_xnor, a, b);
  notif0 u_notif (o_notif, a, en);
  pullup u_pull  (o_pull);
endmodule

// `alias` makes two names one net with no statement between them.
module cx_alias (input logic [7:0] in_side, output logic [7:0] out_side);
  // Both sides have to be nets: an alias is a wiring statement, not an
  // assignment, which is the reason it is here at all.
  wire [7:0] left, right;
  alias left = right;
  assign left = in_side;
  assign out_side = right;
endmodule

// An assertion reads signals; whether it counts as a load is Questa's call,
// but the reader must not fall over on the construct.
module cx_assert (input logic clk, input logic req, input logic ack);
  a_handshake: assert property (@(posedge clk) req |-> ##[0:3] ack);
  always_comb begin
    if (req && ack) begin
      // an immediate assertion, which is a statement reading both
      assert (req !== 1'bx);
    end
  end
endmodule

// A packed array of structs as a port, and a parameter of type.
module cx_struct_array #(parameter type T = cx_pkg::req_t) (
    input  T [1:0] pair,
    output logic [7:0] picked
);
  assign picked = pair[1].addr ^ pair[0].addr;
endmodule

// Two processes writing one variable, which is what a `reg` driven from two
// always blocks looks like to the netlist.
module cx_two_writers (input logic clk, input logic sel, input logic [7:0] d,
                       output logic [7:0] shared);
  always_ff @(posedge clk) if (sel) shared <= d;
  always_ff @(posedge clk) if (!sel) shared <= ~d;
  final $display("shared=%0h", shared);             // a final block reading it
endmodule

// ------------------------------------------------------------------- top

module tb;
  logic clk = 1'b0;
  logic rst_n = 1'b0;
  logic en = 1'b1;
  always #5 clk = ~clk;
  initial begin
    #12 rst_n = 1'b1;
    #200 $finish;
  end

  logic [15:0] stim;
  always_ff @(posedge clk) stim <= stim + 16'd7;

  // -- port forms
  wire        ansi_o_reg;
  wire [7:0]  ansi_o_vec;
  wire        ansi_io;
  cx_ansi u_ansi (
      .i_wire(stim[0]), .i_vec(stim[7:0]),
      .o_reg(ansi_o_reg), .o_wire_vec(ansi_o_vec), .io_net(ansi_io)
  );

  wire [3:0] nonansi_y;
  cx_nonansi u_nonansi (stim[3:0], stim[7:4], nonansi_y);   // positional

  wire pc_hi, pc_lo;
  cx_portconcat u_portconcat (.ext_pair({pc_hi, pc_lo}), .ext_one(stim[8]));

  // -- assignment forms
  wire [15:0] as_whole, as_parts;
  wire [7:0]  as_two;
  wire        as_one;
  cx_assigns u_assigns (
      .src(stim), .idx(stim[3:0]),
      .whole(as_whole), .parts(as_parts), .packed_two(as_two), .one_bit(as_one)
  );

  // -- procedural forms
  wire [7:0] pr_ff, pr_latch, pr_star;
  cx_procedural u_procedural (
      .clk(clk), .rst_n(rst_n), .en(en), .d(stim[7:0]),
      .q_ff(pr_ff), .q_latch(pr_latch), .q_star(pr_star)
  );

  // -- net types
  wire nets_wand;
  cx_nets u_nets (.a(stim[0]), .b(stim[1]), .wand_out(nets_wand));

  // -- struct and union ports
  cx_pkg::wrapped_t w_in;
  cx_pkg::view_t    u_in;
  cx_pkg::req_t     r_out;
  wire [7:0]        struct_addr;
  assign w_in.req.addr = stim[7:0];
  assign w_in.req.len  = stim[11:8];
  assign w_in.req.valid = stim[12];
  assign w_in.tag = stim[14:13];
  assign u_in.raw = stim[12:0];
  cx_struct u_struct (.in_w(w_in), .in_u(u_in), .out_r(r_out), .out_addr(struct_addr));

  // -- generates
  wire [7:0] gen_named, gen_unnamed, gen_if, gen_case;
  cx_generate u_generate (
      .clk(clk), .d(stim[7:0]),
      .sum_named(gen_named), .sum_unnamed(gen_unnamed),
      .from_if(gen_if), .from_case(gen_case)
  );

  // -- instance arrays and gates
  wire [3:0] arr_y;
  wire       arr_and, arr_or, arr_not, arr_bufif;
  cx_arrays u_arrays (
      .a(stim[3:0]), .b(stim[7:4]), .y(arr_y),
      .g_and(arr_and), .g_or(arr_or), .g_not(arr_not), .g_bufif(arr_bufif)
  );

  // -- interfaces: single, whole, and an array
  cx_if bus (.clk(clk));
  cx_if bus_arr [2] (.clk(clk));
  wire [7:0] if_seen;
  wire       if_saw_rdy;
  wire [1:0] if_any;
  cx_if_src   u_if_src   (.bus(bus), .payload(stim[7:0]));
  cx_if_snk   u_if_snk   (.bus(bus), .seen(if_seen));
  cx_if_whole u_if_whole (.bus(bus), .saw_rdy(if_saw_rdy));
  cx_if_array u_if_array (.bus(bus_arr), .any(if_any));
  // The array's own members need a driver each, through a modport port.
  cx_if_src u_if_src_a0 (.bus(bus_arr[0]), .payload(stim[7:0]));
  cx_if_src u_if_src_a1 (.bus(bus_arr[1]), .payload(stim[15:8]));

  // -- hierarchical reference across instances
  wire [7:0] hier_visible, hier_copied;
  cx_hier_target u_hier_target (.clk(clk), .visible(hier_visible));
  cx_hier_reader u_hier_reader (.clk(clk), .copied(hier_copied));

  // -- wildcard connection: the names match, so `.*` connects them
  logic       i_wire;
  logic [7:0] i_vec;
  logic       o_reg;
  wire [7:0]  o_wire_vec;
  wire        io_net;
  assign i_wire = stim[9];
  assign i_vec = stim[15:8];
  cx_ansi u_wildcard (.*);

  // -- the second sweep
  wire prim_y;
  cx_prim_user u_prim (.a(stim[0]), .b(stim[1]), .y(prim_y));

  wire [7:0] param_q;
  cx_param u_param (.d(stim[7:0]), .q(param_q));
  defparam u_param.SHIFT = 2;

  cx_unpacked_t up_in;
  logic [3:0][7:0] md_in;
  logic [7:0] arr_in [2];
  wire [7:0] up_sum, md_pick, arr_pick;
  always_comb begin
    up_in.a = stim[7:0];
    up_in.b = stim[15:8];
    md_in = {stim[7:0], stim[15:8], stim[7:0], stim[15:8]};
    arr_in[0] = stim[7:0];
    arr_in[1] = stim[15:8];
  end
  cx_shapes u_shapes (
      .up_in(up_in), .md_in(md_in), .arr_in(arr_in),
      .up_sum(up_sum), .md_pick(md_pick), .arr_pick(arr_pick)
  );

  wire [7:0] nb_q;
  cx_named_block u_named_block (.clk(clk), .d(stim[7:0]), .q(nb_q));

  wire [7:0] loop_bytes [4];
  wire [31:0] loop_rev;
  cx_loops u_loops (.d({stim, stim}), .bytes(loop_bytes), .rev(loop_rev));

  wire [7:0] forced_q;
  cx_forced u_forced (.clk(clk), .sel(stim[3]), .d(stim[7:0]), .q(forced_q));

  cx_meth_if meth ();
  wire [7:0] meth_got;
  cx_meth_user u_meth (.bus(meth), .d(stim[7:0]), .got(meth_got));

  // A hierarchical reference written from here rather than read: the driver
  // of `u_hier_target.forced_from_tb` is a statement in another scope.
  initial begin
    #50 tb.u_hier_target.poked = 8'h11;
  end

  // -- the third sweep
  wire da_y, da_z;
  cx_decl_assign u_decl (.a(stim[0]), .b(stim[1]), .y(da_y), .z(da_z));

  wire sw_x, sw_y;
  assign sw_x = stim[2] ? stim[3] : 1'bz;
  cx_switch u_switch (.x(sw_x), .y(sw_y), .en(stim[4]));

  wire g_nand, g_nor, g_xnor, g_notif, g_pull;
  cx_gates u_gates (
      .a(stim[0]), .b(stim[1]), .en(stim[2]),
      .o_nand(g_nand), .o_nor(g_nor), .o_xnor(g_xnor),
      .o_notif(g_notif), .o_pull(g_pull)
  );

  wire [7:0] alias_out;
  cx_alias u_alias (.in_side(stim[7:0]), .out_side(alias_out));

  cx_assert u_assert (.clk(clk), .req(stim[5]), .ack(stim[6]));

  cx_pkg::req_t [1:0] pair;
  wire [7:0] picked;
  always_comb begin
    pair[0].addr = stim[7:0];
    pair[0].len = stim[11:8];
    pair[0].valid = stim[12];
    pair[1].addr = stim[15:8];
    pair[1].len = stim[3:0];
    pair[1].valid = stim[4];
  end
  cx_struct_array u_struct_array (.pair(pair), .picked(picked));

  wire [7:0] shared_q;
  cx_two_writers u_two_writers (.clk(clk), .sel(stim[7]), .d(stim[7:0]), .shared(shared_q));

  // A hierarchical reference reaching into one element of an instance array,
  // which is a scope the walk has to descend through rather than name.
  wire [3:0] reach_arr;
  assign reach_arr = tb.u_arrays.u_arr[2].y;

  // -- the other language, instantiated directly from here
  wire [7:0] vhdl_q;
  wire [3:0] vhdl_lanes;
  cx_vhdl u_vhdl (
      .clk(clk), .d(stim[7:0]), .sel(stim[6]),
      .q(vhdl_q), .lanes(vhdl_lanes)
  );
endmodule

// A bound instance: connectivity created from outside the target module.
bind cx_procedural cx_bound u_bound (.clk(clk), .watched(d), .seen_hi());
