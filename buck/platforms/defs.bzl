"""Execution platforms: where Buck2 sends an action (docs/BUILD.md)."""

def _execution_platforms_impl(ctx):
    constraints = {}
    for dep in ctx.attrs.constraints:
        for label, value in dep[ConfigurationInfo].constraints.items():
            constraints[label] = value
    configuration = ConfigurationInfo(constraints = constraints, values = {})
    platform = ExecutionPlatformInfo(
        label = ctx.label.raw_target(),
        configuration = configuration,
        executor_config = CommandExecutorConfig(
            # Nothing runs in Buck2's own local executor: every action goes
            # to NativeLink, so its result is cached and it sees only its
            # declared inputs.
            local_enabled = False,
            remote_enabled = True,
            remote_execution_properties = ctx.attrs.properties,
            remote_execution_use_case = "buck2-default",
            remote_output_paths = "output_paths",
        ),
    )
    return [DefaultInfo(), ExecutionPlatformRegistrationInfo(platforms = [platform])]

execution_platforms = rule(
    impl = _execution_platforms_impl,
    attrs = {
        "constraints": attrs.list(attrs.dep(providers = [ConfigurationInfo])),
        "properties": attrs.dict(attrs.string(), attrs.string()),
    },
)
