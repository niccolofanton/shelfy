{{! The user message of a cluster refinement (task "cluster_refine"), one per candidate group. }}
{{! tags: one line per tag of the group, "- tag (neighbor, neighbor)" or "- tag". }}
Raw group of tags, grouped because they often co-occur in the same posts.
In parentheses, for each tag, its most frequent neighbors (context).

{{#if tags}}
{{tags}}
{{/if}}

Return one or more semantically coherent clusters by meaning, using ONLY the tags listed above, verbatim.
Give each cluster a short canonical name. Put the tags that do not belong to any clear theme into 'outliers'.
