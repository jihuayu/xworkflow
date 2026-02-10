#![allow(dead_code)]

use std::fmt::Write;

pub fn build_linear_workflow(node_count: usize, node_type: &str) -> String {
    let mut yaml = String::new();
    let node_count = node_count.max(1);
    writeln!(&mut yaml, "version: \"0.1.0\"").ok();
    writeln!(&mut yaml, "nodes:").ok();
    writeln!(&mut yaml, "  - id: start").ok();
    writeln!(&mut yaml, "    data: {{ type: start, title: Start }}").ok();

    for i in 0..node_count {
        writeln!(&mut yaml, "  - id: n{}", i).ok();
        if node_type == "template-transform" {
            writeln!(&mut yaml, "    data:").ok();
            writeln!(&mut yaml, "      type: {}", node_type).ok();
            writeln!(&mut yaml, "      title: N{}", i).ok();
            writeln!(&mut yaml, "      template: \"hello\"").ok();
        } else {
            writeln!(
                &mut yaml,
                "    data: {{ type: {}, title: N{} }}",
                node_type, i
            )
            .ok();
        }
    }

    writeln!(&mut yaml, "  - id: end").ok();
    writeln!(&mut yaml, "    data: {{ type: end, title: End, outputs: [] }}").ok();

    writeln!(&mut yaml, "edges:").ok();
    writeln!(&mut yaml, "  - source: start").ok();
    writeln!(&mut yaml, "    target: n0").ok();

    for i in 0..(node_count - 1) {
        writeln!(&mut yaml, "  - source: n{}", i).ok();
        writeln!(&mut yaml, "    target: n{}", i + 1).ok();
    }

    writeln!(&mut yaml, "  - source: n{}", node_count - 1).ok();
    writeln!(&mut yaml, "    target: end").ok();

    yaml
}

pub fn build_branch_workflow(branches: usize) -> String {
    let mut yaml = String::new();
    let branches = branches.max(1);
    writeln!(&mut yaml, "version: \"0.1.0\"").ok();
    writeln!(&mut yaml, "nodes:").ok();
    writeln!(&mut yaml, "  - id: start").ok();
    writeln!(&mut yaml, "    data: {{ type: start, title: Start }}").ok();
    writeln!(&mut yaml, "  - id: if1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: if-else").ok();
    writeln!(&mut yaml, "      title: Branch").ok();
    writeln!(&mut yaml, "      cases:").ok();
    for i in 0..branches {
        writeln!(&mut yaml, "        - case_id: c{}", i).ok();
        writeln!(&mut yaml, "          logical_operator: and").ok();
        writeln!(&mut yaml, "          conditions:").ok();
        writeln!(
            &mut yaml,
            "            - variable_selector: [\"start\", \"flag{}\"]",
            i
        )
        .ok();
        writeln!(&mut yaml, "              comparison_operator: is").ok();
        writeln!(&mut yaml, "              value: true").ok();
    }

    for i in 0..branches {
        writeln!(&mut yaml, "  - id: b{}", i).ok();
        writeln!(
            &mut yaml,
            "    data: {{ type: end, title: B{}, outputs: [] }}",
            i
        )
        .ok();
    }

    writeln!(&mut yaml, "edges:").ok();
    writeln!(&mut yaml, "  - source: start").ok();
    writeln!(&mut yaml, "    target: if1").ok();
    for i in 0..branches {
        writeln!(&mut yaml, "  - source: if1").ok();
        writeln!(&mut yaml, "    target: b{}", i).ok();
        writeln!(&mut yaml, "    sourceHandle: c{}", i).ok();
    }
    yaml
}

pub fn build_fanout_workflow(branch_count: usize) -> String {
    let mut yaml = String::new();
    let branch_count = branch_count.max(1);

    writeln!(&mut yaml, "version: \"0.1.0\"").ok();
    writeln!(&mut yaml, "nodes:").ok();
    writeln!(&mut yaml, "  - id: start").ok();
    writeln!(&mut yaml, "    data: {{ type: start, title: Start }}").ok();
    writeln!(&mut yaml, "  - id: if1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: if-else").ok();
    writeln!(&mut yaml, "      title: Fanout").ok();
    writeln!(&mut yaml, "      cases:").ok();
    for i in 0..branch_count {
        writeln!(&mut yaml, "        - case_id: c{}", i).ok();
        writeln!(&mut yaml, "          logical_operator: and").ok();
        writeln!(&mut yaml, "          conditions:").ok();
        writeln!(
            &mut yaml,
            "            - variable_selector: [\"start\", \"flag{}\"]",
            i
        )
        .ok();
        writeln!(&mut yaml, "              comparison_operator: is").ok();
        writeln!(&mut yaml, "              value: true").ok();
    }

    for i in 0..branch_count {
        writeln!(&mut yaml, "  - id: b{}", i).ok();
        writeln!(
            &mut yaml,
            "    data: {{ type: end, title: Branch {}, outputs: [] }}",
            i
        )
        .ok();
    }
    writeln!(&mut yaml, "  - id: b_false").ok();
    writeln!(&mut yaml, "    data: {{ type: end, title: Branch F, outputs: [] }}").ok();

    writeln!(&mut yaml, "edges:").ok();
    writeln!(&mut yaml, "  - source: start").ok();
    writeln!(&mut yaml, "    target: if1").ok();
    for i in 0..branch_count {
        writeln!(&mut yaml, "  - source: if1").ok();
        writeln!(&mut yaml, "    target: b{}", i).ok();
        writeln!(&mut yaml, "    sourceHandle: c{}", i).ok();
    }
    writeln!(&mut yaml, "  - source: if1").ok();
    writeln!(&mut yaml, "    target: b_false").ok();
    writeln!(&mut yaml, "    sourceHandle: \"false\"").ok();

    yaml
}

pub fn build_diamond_workflow(branch_count: usize) -> String {
    let mut yaml = String::new();
    let branch_count = branch_count.max(1);

    writeln!(&mut yaml, "version: \"0.1.0\"").ok();
    writeln!(&mut yaml, "nodes:").ok();
    writeln!(&mut yaml, "  - id: start").ok();
    writeln!(&mut yaml, "    data: {{ type: start, title: Start }}").ok();
    writeln!(&mut yaml, "  - id: if1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: if-else").ok();
    writeln!(&mut yaml, "      title: Split").ok();
    writeln!(&mut yaml, "      cases:").ok();
    for i in 0..branch_count {
        writeln!(&mut yaml, "        - case_id: c{}", i).ok();
        writeln!(&mut yaml, "          logical_operator: and").ok();
        writeln!(&mut yaml, "          conditions:").ok();
        writeln!(
            &mut yaml,
            "            - variable_selector: [\"start\", \"flag{}\"]",
            i
        )
        .ok();
        writeln!(&mut yaml, "              comparison_operator: is").ok();
        writeln!(&mut yaml, "              value: true").ok();
    }

    for i in 0..branch_count {
        writeln!(&mut yaml, "  - id: b{}", i).ok();
        writeln!(&mut yaml, "    data:").ok();
        writeln!(&mut yaml, "      type: template-transform").ok();
        writeln!(&mut yaml, "      title: Branch {}", i).ok();
        writeln!(&mut yaml, "      template: \"b{}: {{ value }}\"", i).ok();
        writeln!(&mut yaml, "      variables:").ok();
        writeln!(&mut yaml, "        - variable: value").ok();
        writeln!(
            &mut yaml,
            "          value_selector: [\"start\", \"query\"]"
        )
        .ok();
    }

    writeln!(&mut yaml, "  - id: b_false").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: template-transform").ok();
    writeln!(&mut yaml, "      title: Branch F").ok();
    writeln!(&mut yaml, "      template: \"b_false: {{ value }}\"").ok();
    writeln!(&mut yaml, "      variables:").ok();
    writeln!(&mut yaml, "        - variable: value").ok();
    writeln!(&mut yaml, "          value_selector: [\"start\", \"query\"]").ok();

    writeln!(&mut yaml, "  - id: join").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: variable-aggregator").ok();
    writeln!(&mut yaml, "      title: Join").ok();
    writeln!(&mut yaml, "      variables:").ok();
    for i in 0..branch_count {
        writeln!(
            &mut yaml,
            "        - [\"b{}\", \"output\"]",
            i
        )
        .ok();
    }
    writeln!(&mut yaml, "        - [\"b_false\", \"output\"]").ok();
    writeln!(&mut yaml, "  - id: end").ok();
    writeln!(&mut yaml, "    data: {{ type: end, title: End, outputs: [] }}").ok();

    writeln!(&mut yaml, "edges:").ok();
    writeln!(&mut yaml, "  - source: start").ok();
    writeln!(&mut yaml, "    target: if1").ok();
    for i in 0..branch_count {
        writeln!(&mut yaml, "  - source: if1").ok();
        writeln!(&mut yaml, "    target: b{}", i).ok();
        writeln!(&mut yaml, "    sourceHandle: c{}", i).ok();
        writeln!(&mut yaml, "  - source: b{}", i).ok();
        writeln!(&mut yaml, "    target: join").ok();
    }
    writeln!(&mut yaml, "  - source: if1").ok();
    writeln!(&mut yaml, "    target: b_false").ok();
    writeln!(&mut yaml, "    sourceHandle: \"false\"").ok();
    writeln!(&mut yaml, "  - source: b_false").ok();
    writeln!(&mut yaml, "    target: join").ok();
    writeln!(&mut yaml, "  - source: join").ok();
    writeln!(&mut yaml, "    target: end").ok();

    yaml
}

pub fn build_deep_branch_chain(depth: usize) -> String {
    let mut yaml = String::new();
    let depth = depth.max(1);

    writeln!(&mut yaml, "version: \"0.1.0\"").ok();
    writeln!(&mut yaml, "nodes:").ok();
    writeln!(&mut yaml, "  - id: start").ok();
    writeln!(&mut yaml, "    data: {{ type: start, title: Start }}").ok();

    for i in 0..depth {
        writeln!(&mut yaml, "  - id: if{}", i + 1).ok();
        writeln!(&mut yaml, "    data:").ok();
        writeln!(&mut yaml, "      type: if-else").ok();
        writeln!(&mut yaml, "      title: IF {}", i + 1).ok();
        writeln!(&mut yaml, "      cases:").ok();
        writeln!(&mut yaml, "        - case_id: c{}", i + 1).ok();
        writeln!(&mut yaml, "          logical_operator: and").ok();
        writeln!(&mut yaml, "          conditions:").ok();
        writeln!(
            &mut yaml,
            "            - variable_selector: [\"start\", \"flag{}\"]",
            i + 1
        )
        .ok();
        writeln!(&mut yaml, "              comparison_operator: is").ok();
        writeln!(&mut yaml, "              value: true").ok();
    }

    writeln!(&mut yaml, "  - id: end").ok();
    writeln!(&mut yaml, "    data: {{ type: end, title: End, outputs: [] }}").ok();

    writeln!(&mut yaml, "edges:").ok();
    writeln!(&mut yaml, "  - source: start").ok();
    writeln!(&mut yaml, "    target: if1").ok();

    for i in 0..depth {
        let current = i + 1;
        let next = i + 2;
        if current < depth + 1 {
            if current < depth {
                writeln!(&mut yaml, "  - source: if{}", current).ok();
                writeln!(&mut yaml, "    target: if{}", next).ok();
                writeln!(&mut yaml, "    sourceHandle: c{}", current).ok();
            } else {
                writeln!(&mut yaml, "  - source: if{}", current).ok();
                writeln!(&mut yaml, "    target: end").ok();
                writeln!(&mut yaml, "    sourceHandle: c{}", current).ok();
            }
            writeln!(&mut yaml, "  - source: if{}", current).ok();
            writeln!(&mut yaml, "    target: end").ok();
            writeln!(&mut yaml, "    sourceHandle: \"false\"").ok();
        }
    }

    yaml
}

pub fn build_realistic_mixed_workflow() -> String {
    let mut yaml = String::new();

    writeln!(&mut yaml, "version: \"0.1.0\"").ok();
    writeln!(&mut yaml, "nodes:").ok();
    writeln!(&mut yaml, "  - id: start").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: start").ok();
    writeln!(&mut yaml, "      title: Start").ok();
    writeln!(&mut yaml, "      variables:").ok();
    writeln!(&mut yaml, "        - variable: query").ok();
    writeln!(&mut yaml, "          label: Query").ok();
    writeln!(&mut yaml, "          type: string").ok();
    writeln!(&mut yaml, "          required: false").ok();
    writeln!(&mut yaml, "        - variable: flag").ok();
    writeln!(&mut yaml, "          label: Flag").ok();
    writeln!(&mut yaml, "          type: boolean").ok();
    writeln!(&mut yaml, "          required: false").ok();

    writeln!(&mut yaml, "  - id: code1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: code").ok();
    writeln!(&mut yaml, "      title: Code 1").ok();
    writeln!(
        &mut yaml,
        "      code: |\n        function main(inputs) {{ return {{ text: inputs.query || \\\"\\\" }}; }}"
    )
    .ok();
    writeln!(&mut yaml, "      language: javascript").ok();
    writeln!(&mut yaml, "      variables:").ok();
    writeln!(&mut yaml, "        - variable: query").ok();
    writeln!(&mut yaml, "          value_selector: [\"start\", \"query\"]").ok();

    writeln!(&mut yaml, "  - id: tpl1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: template-transform").ok();
    writeln!(&mut yaml, "      title: Template").ok();
    writeln!(&mut yaml, "      template: \"Hello {{ name }}\"").ok();
    writeln!(&mut yaml, "      variables:").ok();
    writeln!(&mut yaml, "        - variable: name").ok();
    writeln!(&mut yaml, "          value_selector: [\"code1\", \"text\"]").ok();

    writeln!(&mut yaml, "  - id: if1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: if-else").ok();
    writeln!(&mut yaml, "      title: Check").ok();
    writeln!(&mut yaml, "      cases:").ok();
    writeln!(&mut yaml, "        - case_id: yes").ok();
    writeln!(&mut yaml, "          logical_operator: and").ok();
    writeln!(&mut yaml, "          conditions:").ok();
    writeln!(&mut yaml, "            - variable_selector: [\"start\", \"flag\"]").ok();
    writeln!(&mut yaml, "              comparison_operator: is").ok();
    writeln!(&mut yaml, "              value: true").ok();

    writeln!(&mut yaml, "  - id: code_yes").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: code").ok();
    writeln!(&mut yaml, "      title: Code Yes").ok();
    writeln!(
        &mut yaml,
        "      code: |\n        function main(inputs) {{ return {{ text: \\\"yes\\\" }}; }}"
    )
    .ok();
    writeln!(&mut yaml, "      language: javascript").ok();

    writeln!(&mut yaml, "  - id: code_no").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: code").ok();
    writeln!(&mut yaml, "      title: Code No").ok();
    writeln!(
        &mut yaml,
        "      code: |\n        function main(inputs) {{ return {{ text: \\\"no\\\" }}; }}"
    )
    .ok();
    writeln!(&mut yaml, "      language: javascript").ok();

    writeln!(&mut yaml, "  - id: agg1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: variable-aggregator").ok();
    writeln!(&mut yaml, "      title: Agg").ok();
    writeln!(&mut yaml, "      variables:").ok();
    writeln!(&mut yaml, "        - [\"code_yes\", \"text\"]").ok();
    writeln!(&mut yaml, "        - [\"code_no\", \"text\"]").ok();

    writeln!(&mut yaml, "  - id: ans1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: answer").ok();
    writeln!(&mut yaml, "      title: Answer").ok();
    writeln!(&mut yaml, "      answer: \"Result: {{#agg1.output#}}\"").ok();

    writeln!(&mut yaml, "  - id: end").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: end").ok();
    writeln!(&mut yaml, "      title: End").ok();
    writeln!(&mut yaml, "      outputs:").ok();
    writeln!(&mut yaml, "        - variable: result").ok();
    writeln!(&mut yaml, "          value_selector: [\"ans1\", \"answer\"]").ok();

    writeln!(&mut yaml, "edges:").ok();
    writeln!(&mut yaml, "  - source: start").ok();
    writeln!(&mut yaml, "    target: code1").ok();
    writeln!(&mut yaml, "  - source: code1").ok();
    writeln!(&mut yaml, "    target: tpl1").ok();
    writeln!(&mut yaml, "  - source: tpl1").ok();
    writeln!(&mut yaml, "    target: if1").ok();
    writeln!(&mut yaml, "  - source: if1").ok();
    writeln!(&mut yaml, "    target: code_yes").ok();
    writeln!(&mut yaml, "    sourceHandle: yes").ok();
    writeln!(&mut yaml, "  - source: if1").ok();
    writeln!(&mut yaml, "    target: code_no").ok();
    writeln!(&mut yaml, "    sourceHandle: \"false\"").ok();
    writeln!(&mut yaml, "  - source: code_yes").ok();
    writeln!(&mut yaml, "    target: agg1").ok();
    writeln!(&mut yaml, "  - source: code_no").ok();
    writeln!(&mut yaml, "    target: agg1").ok();
    writeln!(&mut yaml, "  - source: agg1").ok();
    writeln!(&mut yaml, "    target: ans1").ok();
    writeln!(&mut yaml, "  - source: ans1").ok();
    writeln!(&mut yaml, "    target: end").ok();

    yaml
}

pub fn build_iteration_workflow(items: usize, parallel: bool, parallelism: usize) -> String {
    let mut yaml = String::new();

    writeln!(&mut yaml, "version: \"0.1.0\"").ok();
    writeln!(&mut yaml, "nodes:").ok();
    writeln!(&mut yaml, "  - id: start").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: start").ok();
    writeln!(&mut yaml, "      title: Start").ok();
    writeln!(&mut yaml, "      variables:").ok();
    writeln!(&mut yaml, "        - variable: items").ok();
    writeln!(&mut yaml, "          label: Items").ok();
    writeln!(&mut yaml, "          type: array[string]").ok();
    writeln!(&mut yaml, "          required: false").ok();

    writeln!(&mut yaml, "  - id: iter1").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: iteration").ok();
    writeln!(&mut yaml, "      title: Iteration").ok();
    writeln!(&mut yaml, "      input_selector: [\"start\", \"items\"]").ok();
    writeln!(&mut yaml, "      output_variable: results").ok();
    writeln!(&mut yaml, "      parallel: {}", if parallel { "true" } else { "false" }).ok();
    writeln!(&mut yaml, "      parallelism: {}", parallelism).ok();
    writeln!(&mut yaml, "      max_iterations: {}", items.max(1)).ok();
    writeln!(&mut yaml, "      sub_graph:").ok();
    writeln!(&mut yaml, "        nodes:").ok();
    writeln!(&mut yaml, "          - id: sg_start").ok();
    writeln!(&mut yaml, "            type: start").ok();
    writeln!(&mut yaml, "          - id: sg_tpl").ok();
    writeln!(&mut yaml, "            type: template-transform").ok();
    writeln!(&mut yaml, "            data:").ok();
    writeln!(&mut yaml, "              template: \"Item: {{ item }}\"").ok();
    writeln!(&mut yaml, "              variables:").ok();
    writeln!(&mut yaml, "                - variable: item").ok();
    writeln!(&mut yaml, "                  value_selector: [\"__scope__\", \"item\"]").ok();
    writeln!(&mut yaml, "          - id: sg_end").ok();
    writeln!(&mut yaml, "            type: end").ok();
    writeln!(&mut yaml, "            data:").ok();
    writeln!(&mut yaml, "              outputs:").ok();
    writeln!(&mut yaml, "                - variable: output").ok();
    writeln!(&mut yaml, "                  value_selector: [\"sg_tpl\", \"output\"]").ok();
    writeln!(&mut yaml, "        edges:").ok();
    writeln!(&mut yaml, "          - source: sg_start").ok();
    writeln!(&mut yaml, "            target: sg_tpl").ok();
    writeln!(&mut yaml, "          - source: sg_tpl").ok();
    writeln!(&mut yaml, "            target: sg_end").ok();

    writeln!(&mut yaml, "  - id: end").ok();
    writeln!(&mut yaml, "    data:").ok();
    writeln!(&mut yaml, "      type: end").ok();
    writeln!(&mut yaml, "      title: End").ok();
    writeln!(&mut yaml, "      outputs:").ok();
    writeln!(&mut yaml, "        - variable: results").ok();
    writeln!(&mut yaml, "          value_selector: [\"iter1\", \"results\"]").ok();

    writeln!(&mut yaml, "edges:").ok();
    writeln!(&mut yaml, "  - source: start").ok();
    writeln!(&mut yaml, "    target: iter1").ok();
    writeln!(&mut yaml, "  - source: iter1").ok();
    writeln!(&mut yaml, "    target: end").ok();

    yaml
}
