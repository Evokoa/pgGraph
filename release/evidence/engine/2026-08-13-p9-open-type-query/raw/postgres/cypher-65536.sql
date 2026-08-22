SELECT count(*) FROM graph.cypher(replace('MATCH (u@p9_latency_nodes {id: 1})-[@type_1]->(v@p9_latency_nodes) RETURN v', '@', chr(58)), hydrate := false);
