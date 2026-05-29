// real_event.v — exercises non-logic value kinds: `real` variables (printed
// verbatim) and a Verilog `event`, plus a wide 64-bit vector to test bignum
// decimal/hex formatting in fmt_val.
`timescale 1ns/1ps
module real_event;
  real        voltage;
  real        temperature;
  reg  [63:0] wide;
  event       sample;       // named event
  reg         sampled;
  integer     i;

  initial begin
    $dumpfile("real_event.vcd");
    $dumpvars(0, real_event);
    voltage     = 0.0;
    temperature = 25.5;
    wide        = 64'h0000_0000_0000_0000;
    sampled     = 1'b0;

    for (i = 0; i < 8; i = i + 1) begin
      #10;
      voltage     = voltage + 0.25;
      temperature = temperature - 0.5;
      wide        = wide + 64'h0011_2233_4455_6677;
      ->sample;                 // trigger the event
      sampled     = ~sampled;
    end
    #10 $finish;
  end
endmodule
