{module, suspend_reexec_fixture}.  %% version = 0

{exports, [{module_info,0},{module_info,1},{run,1},{run_precomputed,1}]}.

{attributes, []}.

{labels, 16}.


{function, run, 1, 2}.
  {label,1}.
    {line,[{location,"suspend_reexec_fixture.erl",12}]}.
    {func_info,{atom,suspend_reexec_fixture},{atom,run},1}.
  {label,2}.
    {test_heap,{alloc,[{words,1},{floats,0},{funs,1}]},1}.
    {make_fun3,{f,15},0,0,{x,0},{list,[{x,0}]}}.
    {call_only,1,{f,6}}. % invoke/1


{function, run_precomputed, 1, 4}.
  {label,3}.
    {line,[{location,"suspend_reexec_fixture.erl",20}]}.
    {func_info,{atom,suspend_reexec_fixture},{atom,run_precomputed},1}.
  {label,4}.
    {allocate,0,1}.
    {line,[{location,"suspend_reexec_fixture.erl",21}]}.
    {call_ext,1,{extfunc,suspend_reexec_helper,to_ms,1}}.
    {test_heap,{alloc,[{words,1},{floats,0},{funs,1}]},1}.
    {make_fun3,{f,13},0,0,{x,0},{list,[{x,0}]}}.
    {call_last,1,{f,6},0}. % invoke/1


{function, invoke, 1, 6}.
  {label,5}.
    {line,[{location,"suspend_reexec_fixture.erl",25}]}.
    {func_info,{atom,suspend_reexec_fixture},{atom,invoke},1}.
  {label,6}.
    {'%',{var_info,{x,0},[{type,{t_fun,0,any,any}}]}}.
    {allocate,0,1}.
    {line,[{location,"suspend_reexec_fixture.erl",26}]}.
    {call_fun2,{atom,safe},0,{tr,{x,0},{t_fun,0,any,any}}}.
    {test,is_tagged_tuple,{f,7},[{x,0},2,{atom,ok}]}.
    {get_tuple_element,{x,0},1,{x,1}}.
    {test,is_binary,{f,7},[{x,1}]}.
    {line,[{location,"suspend_reexec_fixture.erl",27}]}.
    {gc_bif,byte_size,{f,0},2,[{tr,{x,1},{t_bitstring,8,false}}],{x,0}}.
    {test_heap,3,1}.
    {put_tuple2,{x,0},{list,[{atom,done},{x,0}]}}.
    {deallocate,0}.
    return.
  {label,7}.
    {test_heap,3,1}.
    {put_tuple2,{x,0},{list,[{atom,unexpected},{x,0}]}}.
    {deallocate,0}.
    return.


{function, module_info, 0, 9}.
  {label,8}.
    {line,[]}.
    {func_info,{atom,suspend_reexec_fixture},{atom,module_info},0}.
  {label,9}.
    {move,{atom,suspend_reexec_fixture},{x,0}}.
    {call_ext_only,1,{extfunc,erlang,get_module_info,1}}.


{function, module_info, 1, 11}.
  {label,10}.
    {line,[]}.
    {func_info,{atom,suspend_reexec_fixture},{atom,module_info},1}.
  {label,11}.
    {move,{x,0},{x,1}}.
    {move,{atom,suspend_reexec_fixture},{x,0}}.
    {call_ext_only,2,{extfunc,erlang,get_module_info,2}}.


{function, '-run_precomputed/1-fun-0-', 1, 13}.
  {label,12}.
    {line,[{location,"suspend_reexec_fixture.erl",22}]}.
    {func_info,{atom,suspend_reexec_fixture},
               {atom,'-run_precomputed/1-fun-0-'},
               1}.
  {label,13}.
    {call_ext_only,1,{extfunc,beamr_suspend_reexec_test,sleep,1}}.


{function, '-run/1-fun-0-', 1, 15}.
  {label,14}.
    {line,[{location,"suspend_reexec_fixture.erl",13}]}.
    {func_info,{atom,suspend_reexec_fixture},{atom,'-run/1-fun-0-'},1}.
  {label,15}.
    {allocate,0,1}.
    {line,[{location,"suspend_reexec_fixture.erl",14}]}.
    {call_ext,1,{extfunc,suspend_reexec_helper,to_ms,1}}.
    {call_ext_last,1,{extfunc,beamr_suspend_reexec_test,sleep,1},0}.
