"""The events of a GitHub Actions workflow's top-level `on:` key.

Neither the standard library nor the runner's system Python has a YAML parser, so this
reads the four ways GitHub workflows write `on:` (a scalar, a flow list, a flow map,
and a block of keys or list items). Any other form returns None, and callers treat
None as a failure: it fails closed.
"""
import re

ON = re.compile(r'''^(?:on|"on"|'on'|true)\s*:(.*)$''')
COMMENT = re.compile(r'(^|\s)#.*$')


def _names(items):
    return {item.strip().strip('"\'') for item in items if item.strip()}


def on_events(text):
    """The set of event names, or None when there is no top-level on: or it is in a
    form this does not read."""
    lines = [COMMENT.sub('', line).rstrip() for line in text.splitlines()]
    for i, line in enumerate(lines):
        match = ON.match(line)
        if not match:
            continue
        value = match.group(1).strip()
        if value.startswith('['):
            return _names(value[1:-1].split(',')) if value.endswith(']') else None
        if value.startswith('{'):
            if not value.endswith('}') or '{' in value[1:-1] or '[' in value[1:-1]:
                return None
            return _names(part.split(':')[0] for part in value[1:-1].split(','))
        if value:
            return _names([value])
        block = []
        for below in lines[i + 1:]:
            if below and not below[0].isspace():
                break
            block.append(below)
        body = [b for b in block if b.strip()]
        if not body:
            return None
        indent = len(body[0]) - len(body[0].lstrip())
        top = [b.strip() for b in body if len(b) - len(b.lstrip()) == indent]
        if all(t.startswith('- ') for t in top):
            return _names(t[2:] for t in top)
        if all(re.match(r'''^["']?[A-Za-z_]+["']?\s*:''', t) for t in top):
            return _names(t.split(':')[0] for t in top)
        return None
    return None
