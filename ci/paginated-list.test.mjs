import assert from 'node:assert/strict'
import { test } from 'node:test'
import { computed, effectScope, nextTick, ref } from 'vue'
import { usePaginatedList } from '../src/composables/paginated-list.ts'

test('large catalogs mount at most one page and every entry remains reachable', async () => {
  const scope = effectScope()
  try {
    const source = ref(Array.from({ length: 3001 }, (_, i) => i))
    const page = ref(1)
    const { visible, pageSize } = scope.run(() => usePaginatedList(source, page))
    const visited = []
    for (page.value = 1; page.value <= Math.ceil(source.value.length / pageSize); page.value++) {
      assert.ok(visible.value.length <= 50)
      visited.push(...visible.value)
    }
    assert.deepEqual(visited, source.value)

    page.value = 20
    await nextTick()
    assert.equal(page.value, 20)
    assert.equal(visible.value[0], 950)
    // Refreshing a catalog while on a later page must not leave it empty.
    source.value = [100, 101]
    await nextTick()
    assert.equal(page.value, 1)
    assert.deepEqual(visible.value, [100, 101])
  } finally {
    scope.stop()
  }
})

test('filter changes reset pagination, including empty results and clearing search', async () => {
  const scope = effectScope()
  try {
    const source = ref(Array.from({ length: 200 }, (_, i) => String(i)))
    const search = ref('')
    const filtered = computed(() => source.value.filter(item => item.includes(search.value)))
    const page = ref(3)
    const { visible } = scope.run(() => usePaginatedList(filtered, page))
    // Mounting a cached catalog preserves the saved page.
    assert.equal(visible.value[0], '100')
    search.value = '199'
    await nextTick()
    assert.equal(page.value, 1)
    assert.deepEqual(visible.value, ['199'])
    search.value = 'missing'
    await nextTick()
    assert.deepEqual(visible.value, [])
    search.value = ''
    await nextTick()
    assert.equal(page.value, 1)
    assert.equal(visible.value.length, 50)
  } finally {
    scope.stop()
  }
})
