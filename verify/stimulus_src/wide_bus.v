// Adapted from OpenWaveAnalyzer (github.com/neveltyc/OpenWaveAnalyzer),
// verify/designs/wide_bus.v — used here to exercise edge-case value formatting
// (wide all-z/all-x buses, 1-bit [0:0] selects, static signals).

module wide_bus;
  reg clk = 0;
  reg rst_n = 0;
  reg [31:0] wide_data = 32'hzzzzzzzz;
  reg [63:0] big_bus = 64'hxxxxxxxxxxxxxxxx;
  reg [15:0] addr = 16'hzzzz;
  reg [7:0] byte_en = 8'h00;
  reg [2:0] opcode = 3'bxxx;

  always #5 clk = ~clk;

  initial begin
    $dumpfile("wide_bus.vcd");
    $dumpvars(0, wide_bus);
    #2 rst_n = 1;
    #10 wide_data = 32'hDEADBEEF;
    #10 wide_data = 32'hCAFEBABE;
    #10 wide_data = 32'h00000000;
    #10 big_bus = 64'h0123456789ABCDEF;
    #10 addr = 16'h8000;
    #10 byte_en = 8'hFF;
    #10 opcode = 3'b101;
    #10 byte_en = 8'h0F;
    #10 opcode = 3'b010;
    #20 $finish;
  end

  always @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      wide_data <= 32'hzzzzzzzz;
      big_bus <= 64'hxxxxxxxxxxxxxxxx;
      addr <= 16'hzzzz;
      byte_en <= 8'h00;
      opcode <= 3'bxxx;
    end
  end
endmodule
