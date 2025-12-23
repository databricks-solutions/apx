"""Global constants for apx."""

# Port range constants for development servers
DEV_SERVER_PORT_START = 7000
DEV_SERVER_PORT_END = 7999
FRONTEND_PORT_START = 5000
FRONTEND_PORT_END = 5999
BACKEND_PORT_START = 8000
BACKEND_PORT_END = 8999

# Header names for request forwarding
ACCESS_TOKEN_HEADER_NAME = "x-forwarded-access-token"
FORWARDED_USER_HEADER_NAME = "x-forwarded-user"
APX_DEV_PROXY_HEADER = "x-apx-dev-proxy"

# URL/Routing defaults
DEFAULT_API_PREFIX = "/api"
DEFAULT_HOST = "localhost"
APX_MANAGEMENT_PREFIX = "/__apx__"

# Retry configuration
DEFAULT_MAX_RETRIES = 10
