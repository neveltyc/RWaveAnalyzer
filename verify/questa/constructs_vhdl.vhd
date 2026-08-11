-- Copyright (c) 2026 neveltyc
-- released under the MIT License (see LICENSE)
--
-- The other language. `rw_du_tbl` carries a `lang` column and the reader has
-- never been asked about a design unit that is not Verilog, so whether a VHDL
-- module's statements can be found at all is untested rather than known. This
-- is a small one instantiated from the SystemVerilog top: an entity with ports
-- of each direction, a clocked process, a concurrent assignment, a component
-- instantiation, and a for-generate — the same families the .sv fixture covers,
-- in the spelling this language uses.

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity cx_vhdl_leaf is
  generic (WIDTH : integer := 8);
  port (
    clk   : in  std_logic;
    d     : in  std_logic_vector(WIDTH - 1 downto 0);
    q     : out std_logic_vector(WIDTH - 1 downto 0)
  );
end entity;

architecture rtl of cx_vhdl_leaf is
  signal held : std_logic_vector(WIDTH - 1 downto 0);
begin
  -- A clocked process: the driver of `held`.
  seq : process (clk)
  begin
    if rising_edge(clk) then
      held <= d;
    end if;
  end process;

  -- A concurrent assignment: the driver of the port.
  q <= held;
end architecture;

library ieee;
use ieee.std_logic_1164.all;

entity cx_vhdl is
  port (
    clk    : in  std_logic;
    d      : in  std_logic_vector(7 downto 0);
    sel    : in  std_logic;
    q      : out std_logic_vector(7 downto 0);
    lanes  : out std_logic_vector(3 downto 0)
  );
end entity;

architecture rtl of cx_vhdl is
  component cx_vhdl_leaf is
    generic (WIDTH : integer := 8);
    port (
      clk : in  std_logic;
      d   : in  std_logic_vector(WIDTH - 1 downto 0);
      q   : out std_logic_vector(WIDTH - 1 downto 0)
    );
  end component;

  signal inner  : std_logic_vector(7 downto 0);
  signal masked : std_logic_vector(7 downto 0);
begin
  -- A component instantiation, which is where a trace has to cross a port.
  u_leaf : cx_vhdl_leaf
    generic map (WIDTH => 8)
    port map (clk => clk, d => d, q => inner);

  -- A conditional concurrent assignment.
  masked <= inner when sel = '1' else (others => '0');
  q      <= masked;

  -- A for-generate, each branch driving one bit.
  gen_lanes : for i in 0 to 3 generate
    lanes(i) <= inner(i) xor inner(i + 4);
  end generate;
end architecture;
