-module(counter).
-export([version/0, loop_version/0]).

version() -> 2.
loop_version() -> version().
