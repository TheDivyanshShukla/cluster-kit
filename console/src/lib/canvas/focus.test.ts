import { expect, test } from 'bun:test';
import { moveFocus } from './focus';

const g = [{ id: 'a', x: 0, y: 0 }, { id: 'b', x: 10, y: 0 }, { id: 'c', x: 0, y: 10 }, { id: 'd', x: 10, y: 10 }];
test('arrows go to the neighbour; at the edge focus stays', () => {
	expect(moveFocus(g, 'a', 'right')).toBe('b');
	expect(moveFocus(g, 'a', 'down')).toBe('c');
	expect(moveFocus(g, 'd', 'left')).toBe('c');
	expect(moveFocus(g, 'a', 'left')).toBe('a');
	expect(moveFocus(g, null, 'up')).toBe('a');
	expect(moveFocus([], null, 'up')).toBeNull();
});
