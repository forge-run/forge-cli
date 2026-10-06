"""The `{{domain}}::hello` starter op."""

from forge import OpContext, op
from forge_schema.inputs.{{domain}} import HelloRequest, HelloResponse


@op
def hello(ctx: OpContext, req: HelloRequest) -> HelloResponse:
    """Greet the caller by the name they sent."""
    return HelloResponse(message="hello, " + req.name + ", from {{domain}}")
