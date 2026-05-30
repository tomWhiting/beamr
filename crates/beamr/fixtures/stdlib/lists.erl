%% Minimal lists module providing higher-order functions that cannot be
%% implemented as native Rust BIFs (they need to call back into BEAM
%% closures via the interpreter).
%%
%% Compiled with erlc and bundled as a .beam fixture that beamr loads
%% at startup via --dir.  The non-higher-order lists:reverse/1 is also
%% included here for completeness (it duplicates the native stub
%% harmlessly when this module is loaded after native registration).

-module(lists).
-export([map/2, foldr/3, reverse/1, foreach/2]).

map(F, [H|T]) -> [F(H) | map(F, T)];
map(_F, [])   -> [].

foldr(F, Acc, [H|T]) -> F(H, foldr(F, Acc, T));
foldr(_F, Acc, [])   -> Acc.

reverse(L) -> reverse(L, []).
reverse([H|T], Acc) -> reverse(T, [H|Acc]);
reverse([], Acc)     -> Acc.

foreach(F, [H|T]) -> F(H), foreach(F, T);
foreach(_F, [])   -> ok.
