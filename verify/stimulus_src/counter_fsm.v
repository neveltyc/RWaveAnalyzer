// counter_fsm.v — a free-running counter driving a small FSM, with a few
// multi-bit buses. Exercises: scalar toggling (clk/rst), vector values
// (counter, state), rise/fall edge counting, static signals.
`timescale 1ns/1ps
module counter_fsm;
  reg        clk;
  reg        rst_n;
  reg  [7:0] counter;
  reg  [2:0] state;
  reg  [15:0] accum;
  wire       tick;
  reg        enable;
  reg  [3:0] const_cfg;   // set once, then static

  localparam S_IDLE = 3'd0, S_RUN = 3'd1, S_HOLD = 3'd2, S_DONE = 3'd3;

  assign tick = (counter == 8'hFF);

  // clock: 10ns period
  initial begin
    clk = 1'b0;
    forever #5 clk = ~clk;
  end

  initial begin
    $dumpfile("counter_fsm.vcd");
    $dumpvars(0, counter_fsm);
    rst_n     = 1'b0;
    counter   = 8'h00;
    state     = S_IDLE;
    accum     = 16'h0000;
    enable    = 1'b0;
    const_cfg = 4'hA;     // becomes static after t=0

    #12 rst_n  = 1'b1;
    #8  enable = 1'b1;

    // run for a while
    #400 enable = 1'b0;
    #40  $finish;
  end

  // counter
  always @(posedge clk) begin
    if (!rst_n)      counter <= 8'h00;
    else if (enable) counter <= counter + 8'd1;
  end

  // accumulator (wider bus, changes less often)
  always @(posedge clk) begin
    if (!rst_n)            accum <= 16'h0000;
    else if (enable && counter[1:0] == 2'b00)
                           accum <= accum + 16'd7;
  end

  // simple FSM
  always @(posedge clk) begin
    if (!rst_n) state <= S_IDLE;
    else begin
      case (state)
        S_IDLE: if (enable)        state <= S_RUN;
        S_RUN:  if (counter > 8'd200) state <= S_HOLD;
        S_HOLD: if (tick)          state <= S_DONE;
        S_DONE:                    state <= S_IDLE;
        default:                   state <= S_IDLE;
      endcase
    end
  end
endmodule
