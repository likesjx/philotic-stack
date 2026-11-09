"""One synthetic write and its readback; no retry or payload output."""
import json
import sys
from launch_graph import read_private
from probe_graph import query, USER

def perform(factory,password,phase):
    if phase=='seed':
        rows=query(factory,USER,password,"MERGE (n:SyntheticLayoutProbe {id:'fixed'}) RETURN count(n)")
    elif phase=='observe':
        rows=query(factory,USER,password,"MATCH (n:SyntheticLayoutProbe {id:'fixed'}) RETURN count(n)")
    else:raise ValueError('phase')
    if rows!=[[1]]: raise ValueError('result')

if __name__=='__main__':
    try:
        from neo4j import GraphDatabase
        if len(sys.argv)!=3:raise ValueError('usage')
        data=json.loads(read_private(sys.argv[1]))
        if set(data)!={'password','replacement'}:raise ValueError('credentials')
        perform(GraphDatabase.driver,data['password'],sys.argv[2])
        print('graph_layout:'+sys.argv[2]+':passed')
    except Exception:
        print('graph_layout:probe:failed_or_unknown')
        sys.exit(1)
