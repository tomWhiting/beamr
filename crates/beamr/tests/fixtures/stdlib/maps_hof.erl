-module(maps_hof).
-export([map_inc/0, compiled_entry/0]).

map_inc() ->
    maps:map(fun(_K, V) -> V + 1 end, #{a => 1}).

compiled_entry() ->
    map_inc().
