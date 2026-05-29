// handshake_proto.v — a valid/ready data handshake with back-pressure. Built
// to exercise `search`: interval mode (valid=1), segment mode (--show data),
// event mode (--changed data), and multi-condition (valid=1,ready=1).
`timescale 1ns/1ps
module handshake_proto;
  reg        clk;
  reg        rst_n;
  reg        valid;
  reg        ready;
  reg  [7:0] data;
  wire       fire;          // a beat completes when valid & ready
  reg  [7:0] beats;         // count of completed transfers

  assign fire = valid & ready;

  initial begin
    $dumpfile("handshake_proto.vcd");
    $dumpvars(0, handshake_proto);
    clk = 0; rst_n = 0; valid = 0; ready = 0; data = 8'h00; beats = 8'h00;
    #12 rst_n = 1;

    // sender presents data
    #8  valid = 1; data = 8'h10;
    #10 ready = 1;             // first beat fires
    #10 data  = 8'h20;         // next word while ready high
    #10 ready = 0;             // back-pressure
    #20 ready = 1;             // resume
    #10 valid = 0;             // sender idle
    #10 valid = 1; data = 8'h30;
    #10 data  = 8'h31;
    #10 valid = 0; ready = 0;
    #20 $finish;
  end

  always #5 clk = ~clk;

  always @(posedge clk) begin
    if (!rst_n) beats <= 8'h00;
    else if (fire) beats <= beats + 8'd1;
  end
endmodule
