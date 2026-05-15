const svg = d3.select("svg");
const width = () => window.innerWidth;
const height = () => window.innerHeight - 48;
const color = d3.scaleOrdinal(d3.schemeTableau10);
let graph;
let link;
let node;
let label;
let simulation;
let activeCommunities = new Set();

fetch("/api/graph").then(r => r.json()).then(g => {
  graph = g;
  graph.nodes.forEach(n => activeCommunities.add(n.community));
  renderLegend();
  render();
});

function renderLegend() {
  const ids = [...activeCommunities].sort((a, b) => a - b);
  d3.select("#legend").selectAll("button").data(ids).join("button")
    .attr("class", "chip")
    .style("border-color", d => color(d))
    .text(d => "community " + d)
    .on("click", function(_, d) {
      if (activeCommunities.has(d)) activeCommunities.delete(d);
      else activeCommunities.add(d);
      d3.select(this).classed("off", !activeCommunities.has(d));
      updateVisibility();
    });
}

function render() {
  const links = graph.links.map(d => ({ ...d }));
  const nodes = graph.nodes.map(d => ({ ...d }));
  simulation = d3.forceSimulation(nodes)
    .force("link", d3.forceLink(links).id(d => d.id).distance(55))
    .force("charge", d3.forceManyBody().strength(-130))
    .force("center", d3.forceCenter(width() / 2, height() / 2))
    .force("collide", d3.forceCollide(d => 6 + Math.sqrt(d.degree + 1) * 2));

  link = svg.append("g").selectAll("line").data(links).join("line")
    .attr("class", "link")
    .attr("stroke-width", d => 0.7 + d.confidence);
  node = svg.append("g").selectAll("circle").data(nodes).join("circle")
    .attr("class", "node")
    .attr("r", d => 4 + Math.sqrt(d.degree + 1) * 1.5)
    .attr("fill", d => color(d.community))
    .call(drag(simulation))
    .on("click", (_, d) => focus(d.id));
  node.append("title").text(d => d.qname);
  label = svg.append("g").selectAll("text").data(nodes.filter(d => d.degree > 1)).join("text")
    .attr("font-size", 10)
    .text(d => d.label);

  simulation.on("tick", () => {
    link
      .attr("x1", d => d.source.x)
      .attr("y1", d => d.source.y)
      .attr("x2", d => d.target.x)
      .attr("y2", d => d.target.y);
    node.attr("cx", d => d.x).attr("cy", d => d.y);
    label.attr("x", d => d.x + 8).attr("y", d => d.y + 3);
  });
}

function updateVisibility() {
  node.style("display", d => activeCommunities.has(d.community) ? null : "none");
  label.style("display", d => activeCommunities.has(d.community) ? null : "none");
  link.style("display", d => {
    return activeCommunities.has(d.source.community) && activeCommunities.has(d.target.community)
      ? null
      : "none";
  });
}

document.getElementById("q").addEventListener("input", async e => {
  const q = e.target.value.trim();
  if (!q) {
    node.attr("stroke", "#fff").attr("stroke-width", 1.5);
    d3.select("#status").text("");
    return;
  }
  const res = await fetch("/api/search?q=" + encodeURIComponent(q)).then(r => r.json());
  const ids = new Set(res.hits.map(h => h.id));
  d3.select("#status").text(res.hits.length + " hits");
  node
    .attr("stroke", d => ids.has(d.id) ? "#111" : "#fff")
    .attr("stroke-width", d => ids.has(d.id) ? 3 : 1.5);
});

function focus(id) {
  const neighbors = new Set([id]);
  graph.links.forEach(l => {
    const s = l.source.id ?? l.source;
    const t = l.target.id ?? l.target;
    if (s === id) neighbors.add(t);
    if (t === id) neighbors.add(s);
  });
  node.attr("opacity", d => neighbors.has(d.id) ? 1 : .15);
  link.attr("opacity", d => neighbors.has(d.source.id) && neighbors.has(d.target.id) ? 1 : .08);
}

function drag(sim) {
  return d3.drag()
    .on("start", (event, d) => {
      if (!event.active) sim.alphaTarget(.3).restart();
      d.fx = d.x;
      d.fy = d.y;
    })
    .on("drag", (event, d) => {
      d.fx = event.x;
      d.fy = event.y;
    })
    .on("end", (event, d) => {
      if (!event.active) sim.alphaTarget(0);
      d.fx = null;
      d.fy = null;
    });
}
