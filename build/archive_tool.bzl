"""A program inside a downloaded archive, runnable with `$(exe ...)`."""

def _archive_tool_impl(ctx: AnalysisContext) -> list[Provider]:
    archive = ctx.attrs.archive[DefaultInfo].default_outputs[0]
    # The whole archive rides along, so what the program finds beside
    # itself (protoc's include/) is there too.
    return [
        DefaultInfo(default_output = archive),
        RunInfo(args = cmd_args(archive.project(ctx.attrs.path), hidden = archive)),
    ]

# `archive` is built for the machine the program runs on, so a select in
# it picks that machine's download.
archive_tool = rule(
    impl = _archive_tool_impl,
    attrs = {
        "archive": attrs.exec_dep(),
        "path": attrs.string(),
    },
)
