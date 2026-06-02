-module(hot_on_load).
-on_load(init/0).
-export([init/0, version/0]).

init() -> ok.
version() -> 1.
