`timescale 1ns / 1ps
// Spike fixture: exercises the driver/load shapes the trace feature must handle.
interface bus_if(input logic clk);
  logic        vld;
  logic [7:0]  data;
  modport mst (output vld, output data);
  modport slv (input  vld, input  data);
endinterface

module alu (
  input  logic        clk,
  input  logic        rst_n,
  input  logic [7:0]  op_a,
  input  logic [7:0]  op_b,
  input  logic        en,
  output logic [7:0]  res
);
  logic [7:0] res_q;
  logic [7:0] sum;

  assign sum = op_a + op_b;          // continuous assign driver

  always_ff @(posedge clk or negedge rst_n) begin   // always_ff driver + async reset
    if (!rst_n)      res_q <= 8'h00;
    else if (en)     res_q <= sum;
  end

  assign res = res_q;                // port driven by assign
endmodule

module core (
  input  logic       clk,
  input  logic       rst_n,
  bus_if.slv         b,
  output logic [7:0] out
);
  typedef enum logic [1:0] { IDLE, RUN, DONE } state_e;
  state_e state, state_n;

  logic [7:0] acc;

  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n) state <= IDLE;
    else        state <= state_n;

  always_comb begin                  // always_comb driver, case-based
    state_n = state;
    unique case (state)
      IDLE: if (b.vld) state_n = RUN;
      RUN : state_n = DONE;
      DONE: state_n = IDLE;
    endcase
  end

  alu u_alu (
    .clk(clk), .rst_n(rst_n),
    .op_a(b.data), .op_b(acc),
    .en(state == RUN),
    .res(out)
  );

  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n) acc <= 8'h00;
    else        acc <= out[3:0];     // part-select load

  // No-reset free-running counter: exactly one statement, which both
  // writes and reads it. The single-hop case the heuristic keys on.
  logic [7:0] free_cnt;
  always_ff @(posedge clk) free_cnt <= free_cnt + 8'h01;

  // Plain self-referential counter: one statement both writes and reads it.
  logic [7:0] self_cnt;
  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n) self_cnt <= 8'h00;
    else        self_cnt <= self_cnt + 8'h01;
endmodule

module tb;
  logic clk = 0;
  logic rst_n = 0;
  always #5 clk = ~clk;

  bus_if b(.clk(clk));
  logic [7:0] out;

  core u_core (.clk(clk), .rst_n(rst_n), .b(b.slv), .out(out));

  // testbench-procedural drive: NPI's RTL fan-in cannot see this
  initial begin
    b.vld  = 0;
    b.data = 8'h00;
    #12 rst_n = 1;
    repeat (4) begin
      @(posedge clk); b.vld <= 1; b.data <= b.data + 8'h11;
    end
    @(posedge clk); b.vld <= 0;
    #50 $finish;
  end

  initial begin
    $fsdbDumpfile("tb.fsdb");
    $fsdbDumpvars(0, tb);
  end
endmodule
