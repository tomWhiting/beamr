-module(spawn_parent).
-export([child_version/0, spawn_child/0, spawn_child_link/0]).

child_version() -> counter:version().
spawn_child() -> spawn(?MODULE, child_version, []).
spawn_child_link() -> spawn_link(?MODULE, child_version, []).
