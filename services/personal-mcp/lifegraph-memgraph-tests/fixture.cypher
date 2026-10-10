CREATE CONSTRAINT ON (n:Goal) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:LifeRootBinding) ASSERT n.key IS UNIQUE;
CREATE (:SyntheticLifeGraphFixture {id:'synthetic-release-v1'});
CREATE (g:Goal {id:'goal:synthetic-root',claim_summary:'synthetic goal',source_policy_manifest:'[]'}),
       (v:Goal {id:'goal:synthetic-variant',claim_summary:'synthetic goal variant',source_policy_manifest:'["goal:synthetic-root"]'}),
       (i:GrowthHypothesis {id:'idea:synthetic',claim_summary:'synthetic idea',source_policy_manifest:'[]'}),
       (o:OpenLoop {id:'openloop:synthetic',claim_summary:'synthetic open loop',source_policy_manifest:'[]'}),
       (p:Person {id:'person:synthetic',claim_summary:'synthetic person',source_policy_manifest:'[]'}),
       (l:Place {id:'place:synthetic',claim_summary:'synthetic place',source_policy_manifest:'[]'}),
       (a:Asset {id:'asset:synthetic',claim_summary:'synthetic thing',source_policy_manifest:'[]'}),
       (c:Goal {id:'goal:synthetic-other',claim_summary:'synthetic claude goal',source_policy_manifest:'[]'}),
       (g)-[:RELATED_TO {source_policy_manifest:'[]'}]->(p);
CREATE (:LifeRootBinding {key:'synthetic-key',root_id:'goal:synthetic-root'});
CREATE (:LifeRootAlias {key:'synthetic-alias',approved:true,root_id:'goal:synthetic-root'});
