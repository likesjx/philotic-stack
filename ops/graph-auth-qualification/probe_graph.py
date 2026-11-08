"""Synthetic auth-only qualification using the maintained Neo4j Bolt driver."""
import json
import sys
from launch_graph import read_private

USER = "beta_fixture"
URI = "bolt://127.0.0.1:7687"
UNAUTHORIZED = "Neo.ClientError.Security.Unauthorized"
MEMGRAPH_UNAUTHENTICATED = "Memgraph.ClientError.Security.Unauthenticated"
AUTH_CODES = {UNAUTHORIZED, MEMGRAPH_UNAUTHENTICATED}

class ProbeFailure(Exception):
    pass

def classify(error, auth_error):
    if isinstance(error, auth_error) and getattr(error, "code", None) in AUTH_CODES:
        return "authentication_denied"
    return "other_failure"

def query(factory, username, password, statement):
    # Implicit transactions: no execute_read/write retry helper.
    auth = None if username is None and password is None else (username, password)
    with factory(URI, auth=auth, connection_timeout=3,
                 max_connection_pool_size=1, max_transaction_retry_time=0) as driver:
        with driver.session() as session:
            result = session.run(statement)
            rows = [list(record.values()) for record in result]
            result.consume()
            return rows

def accepted(factory, password):
    if query(factory, USER, password, "RETURN 1 AS ok") != [[1]]:
        raise ProbeFailure("positive_response")
    if query(factory, USER, password, "SHOW USERS") != [[USER]]:
        raise ProbeFailure("unexpected_initial_users")

def denied(factory, auth_error, username, password, case="unspecified"):
    try:
        query(factory, username, password, "RETURN 1 AS ok")
    except Exception as error:
        if classify(error, auth_error) == "authentication_denied":
            return
        raise ProbeFailure("denial_" + case + "_not_authentication") from None
    raise ProbeFailure("denial_" + case + "_accepted")

def check(factory, auth_error, credentials, phase):
    if (set(credentials) != {"password", "replacement"} or
            any(not isinstance(v, str) or len(v) != 64 for v in credentials.values()) or
            credentials["password"] == credentials["replacement"]):
        raise ProbeFailure("credentials_invalid")
    try:
        accepted(factory, credentials["password"])
    except ProbeFailure:
        raise
    except Exception as error:
        if classify(error, auth_error) == "authentication_denied":
            raise ProbeFailure("positive_authentication_denied") from None
        raise
    denied(factory, auth_error, USER, credentials["replacement"], "wrong_password")
    denied(factory, auth_error, USER, "", "empty_password")
    denied(factory, auth_error, "", "", "empty_basic")
    denied(factory, auth_error, None, None, "no_auth_scheme")  # Bolt scheme=none, not empty basic.
    denied(factory, auth_error, "absent_fixture_user", credentials["password"], "unknown_user")
    # Bracket all denials with positive queries so unavailability is not a pass.
    try:
        accepted(factory, credentials["password"])
    except ProbeFailure:
        raise
    except Exception as error:
        if classify(error, auth_error) == "authentication_denied":
            raise ProbeFailure("positive_authentication_denied") from None
        raise
    if phase not in ("fresh", "restart", "changed_passfile"):
        raise ProbeFailure("unknown_phase")

if __name__ == "__main__":
    try:
        from neo4j import GraphDatabase
        from neo4j.exceptions import Neo4jError, ServiceUnavailable
    except ImportError:
        print("graph_auth:failed:dependency_unavailable")
        sys.exit(1)
    try:
        if len(sys.argv) != 3:
            raise ProbeFailure("usage")
        credentials = json.loads(read_private(sys.argv[1]))
        check(GraphDatabase.driver, Neo4jError, credentials, sys.argv[2])
        print("graph_auth:" + sys.argv[2] + ":passed")
    except ProbeFailure as e:
        print("graph_auth:failed:" + str(e))
        sys.exit(1)
    except ServiceUnavailable:
        print("graph_auth:failed:transport_unavailable")
        sys.exit(1)
    except Exception:
        print("graph_auth:failed:driver_or_query_failure")
        sys.exit(1)
