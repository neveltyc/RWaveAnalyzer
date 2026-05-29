// hier_deep.v — a nested module hierarchy so the analyzer sees several scopes
// at different depths. Exercises scope enumeration in `info`, path prefixes in
// `list --filter`, and signals that alias across instances.
`timescale 1ns/1ps

module leaf(input wire clk, input wire en, output reg [3:0] cnt);
  initial cnt = 4'h0;
  always @(posedge clk) if (en) cnt <= cnt + 4'd1;
endmodule

module mid(input wire clk, input wire en, output wire [3:0] a, output wire [3:0] b);
  leaf u_a(.clk(clk), .en(en),       .cnt(a));
  leaf u_b(.clk(clk), .en(en & a[0]), .cnt(b));
endmodule

module hier_deep;
  reg clk, en;
  wire [3:0] m0_a, m0_b, m1_a, m1_b;

  mid u_m0(.clk(clk), .en(en),        .a(m0_a), .b(m0_b));
  mid u_m1(.clk(clk), .en(en & m0_b[1]), .a(m1_a), .b(m1_b));

  initial begin
    $dumpfile("hier_deep.vcd");
    $dumpvars(0, hier_deep);
    clk = 1'b0; en = 1'b0;
    #7 en = 1'b1;
    #300 $finish;
  end
  always #5 clk = ~clk;
endmodule
