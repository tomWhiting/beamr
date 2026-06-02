-module(closure_test).
-export([make/0, call/1, version/0]).

version() -> 2.
make() -> fun version/0.
call(F) -> F().
