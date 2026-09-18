from dataclasses import dataclass

from forge import OpContext, Value, op


@dataclass
class Greeting:
    message: str


@op("hello")
def hello(ctx: OpContext, input: Value) -> Greeting:
    name: str = input.get("name", "world")
    return Greeting(message="hello, " + name + ", from {{domain}}")
