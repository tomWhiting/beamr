-module(tagged_tuple_patterns).
-export([match/1, nested/1]).

match(X) ->
    case X of
        {ok, V} -> V;
        {error, R} -> R
    end.

nested(X) ->
    case X of
        {outer, {ok, V}} -> V;
        {outer, {error, R}} -> R
    end.
