// White-box regression for the vendored runtime. Run with ASan and UBSan.
#ifndef TS_RUNTIME_SOURCE
#define TS_RUNTIME_SOURCE "../vendor/tree-sitter/src/lib.c"
#endif
#include TS_RUNTIME_SOURCE

static StackNode *node_with_predecessors(StackNodeArray *pool, SubtreePool *subtrees, unsigned count) {
  StackNode *node = stack_node_new(NULL, NULL_SUBTREE, false, 1, pool);
  for (unsigned i = 0; i < count; i++) {
    // Different states prevent merging, so every link is distinct.
    StackNode *previous = stack_node_new(NULL, NULL_SUBTREE, false, i + 2, pool);
    stack_node_add_link(node, (StackLink) {.node = previous, .subtree = NULL_SUBTREE, .is_pending = false}, subtrees);
    stack_node_release(previous, pool, subtrees);
  }
  return node;
}

int main(void) {
  SubtreePool subtrees = ts_subtree_pool_new(0);
  StackNodeArray pool = array_new();

  // Default: upstream behavior, at most eight links.
  StackNode *node = node_with_predecessors(&pool, &subtrees, 129);
  if (node->link_count != 8) return 1;
  stack_node_release(node, &pool, &subtrees);

  // Raised limit: links spill to the heap and are freed before pooled reuse.
  ts_beam_set_stack_link_limit(129);
  for (unsigned round = 0; round < 64; round++) {
    node = node_with_predecessors(&pool, &subtrees, 129);
    if (node->link_count != 129) return 2;
    for (unsigned i = 0; i < 129; i++) {
      if (node->links[i].node->state != i + 2) return 3;
    }
    stack_node_release(node, &pool, &subtrees);
  }

  // Traversal over spilled links stays memory-safe (paths are capped as upstream).
  Stack *stack = ts_stack_new(&subtrees);
  StackHead *head = array_get(&stack->heads, 0);
  stack_node_release(head->node, &stack->node_pool, &subtrees);
  head->node = node_with_predecessors(&stack->node_pool, &subtrees, 129);
  StackSliceArray slices = ts_stack_pop_count(stack, 0, 1);
  if (slices.size == 0 || slices.size > 129) return 4;
  ts_stack_delete(stack);
  ts_beam_set_stack_link_limit(8);

  for (unsigned i = 0; i < pool.size; i++) ts_free(pool.contents[i]);
  array_delete(&pool);
  ts_subtree_pool_delete(&subtrees);
  return 0;
}
