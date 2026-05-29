// xz_tristate.v — exercises 4-state values: x (uninitialized), z (high-Z on a
// tri-state bus), and transitions through unknown. Good for testing fmt_val's
// b<bits> path, condition matching against x/z, and != excluding unknowns.
`timescale 1ns/1ps
module xz_tristate;
  reg        oe;        // output enable for the tri-state driver
  reg  [3:0] drive;
  wire [3:0] bus;       // tri-state: z when !oe
  reg        sel;
  reg  [7:0] partial;   // deliberately left x on some bits early
  reg        flag;

  assign bus = oe ? drive : 4'bzzzz;

  initial begin
    $dumpfile("xz_tristate.vcd");
    $dumpvars(0, xz_tristate);
    // start with several unknowns: oe/drive/sel reg X until assigned
    oe      = 1'b0;     // bus -> zzzz
    sel     = 1'b0;
    flag    = 1'b0;
    drive   = 4'b0000;
    // partial intentionally not initialized -> stays 8'hxx for a while

    #10 oe    = 1'b1;  drive = 4'b1010;   // bus -> 1010
    #10 drive = 4'b1100;
    #10 oe    = 1'b0;                      // bus -> zzzz again
    #10 partial = 8'b0000_1111;            // resolve the unknown
    #10 sel  = 1'b1;
    #10 oe   = 1'b1;  drive = 4'b0001;
    #10 flag = 1'b1;
    #10 partial = 8'b1010_0101;
    #20 $finish;
  end
endmodule
