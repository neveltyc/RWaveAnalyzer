// Adapted from OpenWaveAnalyzer (github.com/neveltyc/OpenWaveAnalyzer),
// verify/designs/edge_cases.v — used here to exercise edge-case value formatting
// (wide all-z/all-x buses, 1-bit [0:0] selects, static signals).

module edge_cases;
  reg clk = 0;
  reg rst_n = 0;
  reg toggle_1 = 0;
  reg toggle_fast = 0;
  reg [0:0] one_bit_bus = 0;
  reg [2:0] three_bit = 3'b000;
  reg static_high = 1;
  reg static_low = 0;

  always #5 clk = ~clk;
  always #10 toggle_fast = ~toggle_fast;

  initial begin
    $dumpfile("edge_cases.vcd");
    $dumpvars(0, edge_cases);
    #2 rst_n = 1;
    #10 toggle_1 = 1;
    #20 toggle_1 = 0;
    #10 toggle_1 = 1;
    #5 toggle_1 = 0;
    #15 three_bit = 3'b101;
    #10 three_bit = 3'b010;
    #10 one_bit_bus = 1;
    #10 one_bit_bus = 0;
    #10 three_bit = 3'b111;
    #20 $finish;
  end
endmodule
